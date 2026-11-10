//! Steam Workshop collections, read with Steam's public web API (no key
//! needed): `GetCollectionDetails` for the items, `GetPublishedFileDetails`
//! for their titles.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use super::steam::STELLARIS_APP_ID;
use crate::mods::collections::{Shared, SharedMod};

const API: &str = "https://api.steampowered.com/ISteamRemoteStorage";
/// Steam's `result` for success; anything else (9: not found) is a failure.
const OK: u32 = 1;
/// A child's `filetype` when it's a collection rather than an item.
const COLLECTION: u32 = 2;
/// Nested collections deeper than this are skipped, in case of a cycle
/// the visited set misses.
const MAX_DEPTH: usize = 8;

/// The collection id in a Workshop link
/// (`…/filedetails/?id=2089433274`) or a bare id.
pub fn parse_id(input: &str) -> Option<u64> {
    let input = input.trim();
    if let Ok(id) = input.parse() {
        return Some(id);
    }
    let query = input.split_once('?')?.1;
    query
        .split(['&', '#'])
        .find_map(|pair| pair.strip_prefix("id="))
        .and_then(|id| id.parse().ok())
}

pub fn page_url(id: u64) -> String {
    format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub id: u64,
    pub collection: bool,
}

/// Every item in the collection in order, nested collections expanded in
/// place, each item once. `children` looks up collections' children; it's
/// given a batch of ids and returns those that are collections.
pub fn expand(root: u64, mut children: impl FnMut(&[u64]) -> Result<HashMap<u64, Vec<Child>>, String>) -> Result<Vec<u64>, String> {
    let mut known = children(&[root])?;
    let Some(top) = known.get(&root).cloned() else { return Err("This isn't a Workshop collection".to_owned()) };
    let mut items = Vec::new();
    let mut seen = HashSet::from([root]);
    // Depth first, so a nested collection's mods sit where it does.
    let mut stack: Vec<(Vec<Child>, usize, usize)> = vec![(top, 0, 0)];
    while let Some((list, at, depth)) = stack.pop() {
        let Some(child) = list.get(at).cloned() else { continue };
        stack.push((list, at + 1, depth));
        if !seen.insert(child.id) {
            continue;
        }
        if !child.collection {
            items.push(child.id);
            continue;
        }
        if depth + 1 >= MAX_DEPTH {
            continue;
        }
        if !known.contains_key(&child.id) {
            known.extend(children(&[child.id])?);
        }
        if let Some(nested) = known.get(&child.id).cloned() {
            stack.push((nested, 0, depth + 1));
        }
    }
    Ok(items)
}

#[derive(Deserialize)]
struct Envelope<T> {
    response: T,
}

#[derive(Deserialize)]
struct CollectionResponse {
    #[serde(default)]
    collectiondetails: Vec<CollectionDetails>,
}

#[derive(Deserialize)]
struct CollectionDetails {
    publishedfileid: String,
    result: u32,
    #[serde(default)]
    children: Vec<RawChild>,
}

#[derive(Deserialize)]
struct RawChild {
    publishedfileid: String,
    #[serde(default)]
    sortorder: i64,
    #[serde(default)]
    filetype: u32,
}

#[derive(Deserialize)]
struct FileResponse {
    #[serde(default)]
    publishedfiledetails: Vec<FileDetails>,
}

#[derive(Deserialize)]
struct FileDetails {
    publishedfileid: String,
    result: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    consumer_app_id: u32,
}

/// The collections among `json`'s entries, children in Steam's sort order.
fn parse_collections(json: &str) -> Result<HashMap<u64, Vec<Child>>, String> {
    let reply: Envelope<CollectionResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    Ok(reply
        .response
        .collectiondetails
        .into_iter()
        .filter(|c| c.result == OK)
        .filter_map(|c| {
            let mut children = c.children;
            children.sort_by_key(|c| c.sortorder);
            let children = children
                .into_iter()
                .filter_map(|c| Some(Child { id: c.publishedfileid.parse().ok()?, collection: c.filetype == COLLECTION }))
                .collect();
            Some((c.publishedfileid.parse().ok()?, children))
        })
        .collect())
}

/// Titles and app ids of the files Steam found.
fn parse_files(json: &str) -> Result<HashMap<u64, (String, u32)>, String> {
    let reply: Envelope<FileResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    Ok(reply
        .response
        .publishedfiledetails
        .into_iter()
        .filter(|f| f.result == OK)
        .filter_map(|f| Some((f.publishedfileid.parse().ok()?, (f.title, f.consumer_app_id))))
        .collect())
}

/// Builds the shared collection from the expanded items and their details;
/// items without a title are named by id.
fn to_shared(root: u64, items: &[u64], files: &HashMap<u64, (String, u32)>) -> Result<Shared, String> {
    let name = match files.get(&root) {
        Some((_, app)) if *app != STELLARIS_APP_ID => return Err("This is a collection for another game".to_owned()),
        Some((title, _)) if !title.trim().is_empty() => title.trim().to_owned(),
        _ => format!("Workshop {root}"),
    };
    let mods = items
        .iter()
        .map(|&id| SharedMod {
            name: files.get(&id).map(|(t, _)| t.trim()).filter(|t| !t.is_empty()).map_or_else(|| format!("Workshop item {id}"), str::to_owned),
            steam_id: Some(id),
            id: format!("mod/ugc_{id}.mod"),
        })
        .collect();
    Ok(Shared { format: 1, name, mods })
}

/// Fetches the collection. Blocks; call off the main thread.
pub fn fetch(root: u64) -> Result<Shared, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(30))).build().into();
    let post = |method: &str, count: &str, ids: &[u64]| -> Result<String, String> {
        let mut form = vec![(count.to_owned(), ids.len().to_string())];
        form.extend(ids.iter().enumerate().map(|(i, id)| (format!("publishedfileids[{i}]"), id.to_string())));
        agent
            .post(&format!("{API}/{method}/v1/"))
            .header("user-agent", &format!("DrukhalManager/{}", env!("CARGO_PKG_VERSION")))
            .send_form(form)
            .map_err(|e| format!("Could not reach Steam: {e}"))?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not read Steam's reply: {e}"))
    };
    let items = expand(root, |ids| parse_collections(&post("GetCollectionDetails", "collectioncount", ids)?))?;
    let mut files = HashMap::new();
    let all: Vec<u64> = std::iter::once(root).chain(items.iter().copied()).collect();
    for batch in all.chunks(100) {
        files.extend(parse_files(&post("GetPublishedFileDetails", "itemcount", batch)?)?);
    }
    to_shared(root, &items, &files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_from_links() {
        assert_eq!(parse_id(" 2089433274 "), Some(2089433274));
        assert_eq!(parse_id("https://steamcommunity.com/sharedfiles/filedetails/?id=2089433274"), Some(2089433274));
        assert_eq!(parse_id("https://steamcommunity.com/workshop/filedetails/?l=german&id=42#comments"), Some(42));
        assert_eq!(parse_id("https://steamcommunity.com/sharedfiles/filedetails/"), None);
        assert_eq!(parse_id("not a link"), None);
    }

    #[test]
    fn parses_steam_replies() {
        let json = r#"{"response":{"result":1,"resultcount":1,"collectiondetails":[
            {"publishedfileid":"10","result":1,"children":[
                {"publishedfileid":"3","sortorder":2,"filetype":0},
                {"publishedfileid":"20","sortorder":1,"filetype":2},
                {"publishedfileid":"1","sortorder":0,"filetype":0}]},
            {"publishedfileid":"99","result":9}]}}"#;
        let parsed = parse_collections(json).unwrap();
        assert_eq!(parsed.len(), 1, "not-found entries are left out");
        let ids: Vec<(u64, bool)> = parsed[&10].iter().map(|c| (c.id, c.collection)).collect();
        assert_eq!(ids, [(1, false), (20, true), (3, false)]);

        let json = r#"{"response":{"result":1,"resultcount":2,"publishedfiledetails":[
            {"publishedfileid":"10","result":1,"title":"Friends' game","consumer_app_id":281990},
            {"publishedfileid":"1","result":9}]}}"#;
        let files = parse_files(json).unwrap();
        assert_eq!(files[&10], ("Friends' game".to_owned(), STELLARIS_APP_ID));
        assert!(!files.contains_key(&1));
        assert!(parse_files("<html>").is_err());
    }

    #[test]
    fn expands_nested_collections_in_place() {
        let item = |id| Child { id, collection: false };
        let nested = |id| Child { id, collection: true };
        let tree = HashMap::from([
            (10, vec![item(1), nested(20), item(3), item(2)]),
            (20, vec![item(2), nested(10), nested(30)]),
            (30, vec![item(4)]),
        ]);
        let mut calls = 0;
        let items = expand(10, |ids| {
            calls += 1;
            Ok(ids.iter().filter_map(|id| Some((*id, tree.get(id)?.clone()))).collect())
        })
        .unwrap();
        assert_eq!(items, [1, 2, 4, 3], "nested items in place, each once, cycles skipped");
        assert_eq!(calls, 3);

        let err = expand(5, |_| Ok(HashMap::new())).unwrap_err();
        assert_eq!(err, "This isn't a Workshop collection");
    }

    /// Talks to Steam: `cargo test -- --ignored fetches_a_real_collection`.
    #[test]
    #[ignore]
    fn fetches_a_real_collection() {
        let shared = fetch(2089433274).unwrap();
        assert!(shared.name.contains("Stellaris"), "{}", shared.name);
        assert!(shared.mods.len() > 10);
        assert!(shared.mods.iter().all(|m| m.steam_id.is_some() && !m.name.starts_with("Workshop item")));
        assert!(fetch(819148835).is_err(), "a mod isn't a collection");
    }

    #[test]
    fn names_the_collection_and_its_mods() {
        let files = HashMap::from([(10, ("  Friends ".to_owned(), STELLARIS_APP_ID)), (1, ("Giga".to_owned(), STELLARIS_APP_ID))]);
        let shared = to_shared(10, &[1, 2], &files).unwrap();
        assert_eq!(shared.name, "Friends");
        assert_eq!(shared.mods[0], SharedMod { name: "Giga".into(), steam_id: Some(1), id: "mod/ugc_1.mod".into() });
        assert_eq!(shared.mods[1].name, "Workshop item 2");

        let other = HashMap::from([(10, ("Skyrim".to_owned(), 489830))]);
        assert!(to_shared(10, &[1], &other).is_err());
    }
}
