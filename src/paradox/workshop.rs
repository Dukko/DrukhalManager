//! Steam Workshop collections, read with Steam's public web API (no key
//! needed): `GetCollectionDetails` for the items, `GetPublishedFileDetails`
//! for their titles and preview images.

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
    #[serde(default)]
    preview_url: String,
    #[serde(default)]
    time_updated: u64,
}

fn parse_times(json: &str) -> Result<HashMap<u64, u64>, String> {
    let reply: Envelope<FileResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    Ok(reply.response.publishedfiledetails.into_iter().filter(|f| f.result == OK && f.time_updated > 0).filter_map(|f| Some((f.publishedfileid.parse().ok()?, f.time_updated))).collect())
}

/// When each of `ids` was last updated on the Workshop, in Unix seconds:
/// newer than the installed copy means Steam hasn't downloaded it yet.
/// Needs no key. Blocks; call off the main thread.
pub fn published_times(ids: &[u64]) -> Result<HashMap<u64, u64>, String> {
    let agent = agent();
    let mut times = HashMap::new();
    for batch in ids.chunks(ITEMS_PER_CALL) {
        times.extend(parse_times(&post(&agent, "GetPublishedFileDetails", "itemcount", batch)?)?);
    }
    Ok(times)
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
            note: None,
        })
        .collect();
    Ok(Shared { format: 1, name, mods })
}

/// The preview image links of the files Steam found, for those that have
/// one on Steam's servers.
fn parse_previews(json: &str) -> Result<HashMap<u64, String>, String> {
    let reply: Envelope<FileResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    Ok(reply
        .response
        .publishedfiledetails
        .into_iter()
        .filter(|f| f.result == OK && f.preview_url.starts_with("https://"))
        .filter_map(|f| Some((f.publishedfileid.parse().ok()?, f.preview_url)))
        .collect())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(30))).build().into()
}

fn user_agent() -> String {
    format!("DrukhalManager/{}", env!("CARGO_PKG_VERSION"))
}

/// Calls one of the API's methods for `ids`, returning Steam's reply.
fn post(agent: &ureq::Agent, method: &str, count: &str, ids: &[u64]) -> Result<String, String> {
    let mut form = vec![(count.to_owned(), ids.len().to_string())];
    form.extend(ids.iter().enumerate().map(|(i, id)| (format!("publishedfileids[{i}]"), id.to_string())));
    agent
        .post(&format!("{API}/{method}/v1/"))
        .header("user-agent", &user_agent())
        .send_form(form)
        .map_err(|e| format!("Could not reach Steam: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read Steam's reply: {e}"))
}

/// Previews bigger than this aren't saved; Steam caps them at 1 MB.
const MAX_PREVIEW: u64 = 4 * 1024 * 1024;

/// Downloads the Workshop preview images of `ids` into `dir`, each named by
/// its id, and returns the ids saved. Items without a preview, or whose
/// download fails, are skipped: a thumbnail isn't worth an error. Blocks;
/// call off the main thread.
pub fn download_previews(ids: &[u64], dir: &std::path::Path) -> Vec<u64> {
    let agent = agent();
    let mut saved = Vec::new();
    if std::fs::create_dir_all(dir).is_err() {
        return saved;
    }
    for batch in ids.chunks(100) {
        let Ok(previews) = post(&agent, "GetPublishedFileDetails", "itemcount", batch).and_then(|json| parse_previews(&json)) else { continue };
        for (id, url) in previews {
            let bytes = agent
                .get(&url)
                .header("user-agent", &user_agent())
                .call()
                .ok()
                .and_then(|mut reply| reply.body_mut().with_config().limit(MAX_PREVIEW).read_to_vec().ok());
            let Some(bytes) = bytes.filter(|b| !b.is_empty()) else { continue };
            let path = dir.join(id.to_string());
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path)).is_ok() {
                saved.push(id);
            }
        }
    }
    saved
}

/// Items asked about per call.
pub const ITEMS_PER_CALL: usize = 100;

#[derive(Deserialize)]
struct DetailsResponse {
    #[serde(default)]
    publishedfiledetails: Vec<Details>,
}

#[derive(Deserialize)]
struct Details {
    publishedfileid: String,
    result: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    children: Vec<DetailsChild>,
}

#[derive(Deserialize)]
struct DetailsChild {
    publishedfileid: String,
}

/// Required items and titles from an `IPublishedFileService/GetDetails`
/// reply: item -> (title, required item ids).
fn parse_details(json: &str) -> Result<HashMap<u64, (String, Vec<u64>)>, String> {
    let reply: Envelope<DetailsResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    Ok(reply
        .response
        .publishedfiledetails
        .into_iter()
        .filter(|d| d.result == OK)
        .filter_map(|d| Some((d.publishedfileid.parse().ok()?, (d.title, d.children.iter().filter_map(|c| c.publishedfileid.parse().ok()).collect()))))
        .collect())
}

fn details(agent: &ureq::Agent, key: &str, ids: &[u64]) -> Result<HashMap<u64, (String, Vec<u64>)>, String> {
    let mut query = vec![("key".to_owned(), key.to_owned()), ("includechildren".to_owned(), "true".to_owned())];
    query.extend(ids.iter().enumerate().map(|(i, id)| (format!("publishedfileids[{i}]"), id.to_string())));
    let json = agent
        .get("https://api.steampowered.com/IPublishedFileService/GetDetails/v1/")
        .header("user-agent", &user_agent())
        .query_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(401 | 403) => "Steam didn't accept the Steam Web API key".to_owned(),
            e => format!("Could not reach Steam: {e}"),
        })?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Could not read Steam's reply: {e}"))?;
    parse_details(&json)
}

/// The Workshop items each of `ids` requires (its "Required items"), with
/// their titles, for up to [`ITEMS_PER_CALL`] of them. The public API
/// doesn't list them, so this uses Steam's Web API with the user's own key.
/// Blocks; call off the main thread.
pub fn requirements(key: &str, ids: &[u64]) -> Result<HashMap<u64, Vec<(u64, String)>>, String> {
    let agent = agent();
    let ids = &ids[..ids.len().min(ITEMS_PER_CALL)];
    let items = details(&agent, key, ids)?;
    let wanted: Vec<u64> = items.values().flat_map(|(_, c)| c.iter().copied()).collect::<HashSet<_>>().into_iter().collect();
    let mut titles: HashMap<u64, String> = HashMap::new();
    for batch in wanted.chunks(ITEMS_PER_CALL) {
        titles.extend(details(&agent, key, batch)?.into_iter().map(|(id, (title, _))| (id, title)));
    }
    let name = |id: u64| titles.get(&id).map(|t| t.trim()).filter(|t| !t.is_empty()).map_or_else(|| format!("Workshop item {id}"), str::to_owned);
    // Items Steam doesn't know (removed, hidden) require nothing to get.
    Ok(ids.iter().map(|id| (*id, items.get(id).map(|(_, c)| c.iter().map(|&r| (r, name(r))).collect()).unwrap_or_default())).collect())
}


/// A Workshop item that lists some mod among its required items, such as
/// a compatibility patch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct Dependent {
    pub id: u64,
    pub title: String,
    /// Its required items.
    pub requires: Vec<u64>,
    pub subscriptions: u64,
    /// When its author last updated it, in Unix seconds.
    pub updated: u64,
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    total: u64,
    #[serde(default)]
    next_cursor: Option<String>,
    #[serde(default)]
    publishedfiledetails: Vec<QueryItem>,
}

#[derive(Deserialize)]
struct QueryItem {
    publishedfileid: String,
    #[serde(default = "ok")]
    result: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    subscriptions: u64,
    #[serde(default)]
    time_updated: u64,
    #[serde(default)]
    tags: Vec<QueryTag>,
    #[serde(default)]
    children: Vec<DetailsChild>,
}

#[derive(Deserialize)]
struct QueryTag {
    #[serde(default)]
    tag: String,
}

fn ok() -> u32 {
    OK
}

/// One page of a `QueryFiles` reply: the items, how many there are in
/// all, and the cursor for the next page.
fn parse_dependents(json: &str) -> Result<(Vec<Dependent>, u64, Option<String>), String> {
    let reply: Envelope<QueryResponse> = serde_json::from_str(json).map_err(|e| format!("Unexpected reply from Steam: {e}"))?;
    let r = reply.response;
    let items = r
        .publishedfiledetails
        .into_iter()
        .filter(|d| d.result == OK)
        .filter_map(|d| {
            Some(Dependent {
                id: d.publishedfileid.parse().ok()?,
                title: d.title.trim().to_owned(),
                requires: d.children.iter().filter_map(|c| c.publishedfileid.parse().ok()).collect(),
                subscriptions: d.subscriptions,
                updated: d.time_updated,
                tags: d.tags.into_iter().map(|t| t.tag).filter(|t| !t.is_empty()).collect(),
            })
        })
        .collect();
    Ok((items, r.total, r.next_cursor))
}

/// Pages asked for at most per mod; Gigastructural Engineering, among the
/// most patched, has about 300 dependents.
const MAX_PAGES: usize = 20;

/// The Workshop items that list `id` among their required items, each
/// with its own required items. Uses Steam's Web API with the user's own
/// key. Blocks; call off the main thread.
pub fn dependents(key: &str, id: u64) -> Result<Vec<Dependent>, String> {
    let agent = agent();
    let mut found = Vec::new();
    let mut cursor = "*".to_owned();
    for _ in 0..MAX_PAGES {
        let query = [
            ("key", key.to_owned()),
            ("appid", STELLARIS_APP_ID.to_string()),
            ("child_publishedfileid", id.to_string()),
            ("return_details", "true".to_owned()),
            ("return_children", "true".to_owned()),
            ("return_tags", "true".to_owned()),
            ("numperpage", ITEMS_PER_CALL.to_string()),
            ("cursor", cursor.clone()),
        ];
        let json = agent
            .get("https://api.steampowered.com/IPublishedFileService/QueryFiles/v1/")
            .header("user-agent", &user_agent())
            .query_pairs(query.iter().map(|(k, v)| (*k, v.as_str())))
            .call()
            .map_err(|e| match e {
                ureq::Error::StatusCode(401 | 403) => "Steam didn't accept the Steam Web API key".to_owned(),
                e => format!("Could not reach Steam: {e}"),
            })?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("Could not read Steam's reply: {e}"))?;
        let (items, total, next) = parse_dependents(&json)?;
        let last = items.is_empty();
        found.extend(items);
        match next {
            Some(next) if !last && next != cursor && (found.len() as u64) < total => cursor = next,
            _ => break,
        }
    }
    // The mod itself never needs patching against itself.
    found.retain(|d| d.id != id);
    Ok(found)
}

/// Fetches the collection. Blocks; call off the main thread.
pub fn fetch(root: u64) -> Result<Shared, String> {
    let agent = agent();
    let post = |method: &str, count: &str, ids: &[u64]| post(&agent, method, count, ids);
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
    fn reads_required_items_from_details() {
        let json = r#"{"response":{"publishedfiledetails":[
            {"result":1,"publishedfileid":"2966277231","title":"MECR+UIOD","children":[
                {"publishedfileid":"2652578379","sortorder":0,"file_type":0},
                {"publishedfileid":"1623423360","sortorder":1,"file_type":0}]},
            {"result":1,"publishedfileid":"3090328185","title":"No requirements"},
            {"result":9,"publishedfileid":"5"}]}}"#;
        let parsed = parse_details(json).unwrap();
        assert_eq!(parsed[&2966277231], ("MECR+UIOD".to_owned(), vec![2652578379, 1623423360]));
        assert!(parsed[&3090328185].1.is_empty());
        assert!(!parsed.contains_key(&5));
    }

    #[test]
    fn reads_dependents_from_queries() {
        let json = r#"{"response":{"total":3,"next_cursor":"AoJ","publishedfiledetails":[
            {"result":1,"publishedfileid":"3001","title":" GPM + EMEX Compatibility Patch ","subscriptions":5120,"time_updated":1730000000,
             "tags":[{"tag":"Compatibility","display_name":"Compatibility"},{"tag":"Gameplay"}],
             "children":[{"publishedfileid":"10","sortorder":0,"file_type":0},{"publishedfileid":"20","sortorder":1,"file_type":0}]},
            {"publishedfileid":"3002","title":"No result field"},
            {"result":9,"publishedfileid":"3003"}]}}"#;
        let (items, total, next) = parse_dependents(json).unwrap();
        assert_eq!((total, next.as_deref()), (3, Some("AoJ")));
        assert_eq!(items.len(), 2, "missing items are left out");
        assert_eq!(items[0], Dependent { id: 3001, title: "GPM + EMEX Compatibility Patch".into(), requires: vec![10, 20], subscriptions: 5120, updated: 1730000000, tags: vec!["Compatibility".into(), "Gameplay".into()] });
        assert!(items[1].requires.is_empty());
    }

    /// Talks to Steam with a key: `STEAM_KEY=… cargo test -- --ignored finds_real_dependents --nocapture`.
    #[test]
    #[ignore]
    fn finds_real_dependents() {
        let key = std::env::var("STEAM_KEY").expect("STEAM_KEY");
        // UI Overhaul Dynamic.
        let found = dependents(&key, 1623423360).unwrap();
        println!("{} dependents, e.g. {:?}", found.len(), found.iter().take(3).map(|d| &d.title).collect::<Vec<_>>());
        assert!(found.len() > ITEMS_PER_CALL, "more than one page");
        assert!(found.iter().all(|d| d.requires.contains(&1623423360)));
        let ids: HashSet<u64> = found.iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), found.len(), "no item twice across pages");
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

        let json = r#"{"response":{"result":1,"resultcount":3,"publishedfiledetails":[
            {"publishedfileid":"10","result":1,"preview_url":"https://images.steamusercontent.com/ugc/1/2/"},
            {"publishedfileid":"11","result":1,"preview_url":""},
            {"publishedfileid":"12","result":1,"preview_url":"http://example.com/x.png"}]}}"#;
        let previews = parse_previews(json).unwrap();
        assert_eq!(previews.len(), 1, "only items with an https preview");
        assert_eq!(previews[&10], "https://images.steamusercontent.com/ugc/1/2/");
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

    /// Talks to Steam: `cargo test -- --ignored downloads_real_previews`.
    #[test]
    #[ignore]
    fn downloads_real_previews() {
        let dir = std::env::temp_dir().join(format!("drukhal-previews-{}", std::process::id()));
        let shared = fetch(2089433274).unwrap();
        let ids: Vec<u64> = shared.mods.iter().filter_map(|m| m.steam_id).take(3).collect();
        let saved = download_previews(&ids, &dir);
        assert!(!saved.is_empty());
        assert!(gtk::gdk_pixbuf::Pixbuf::from_file(dir.join(saved[0].to_string())).is_ok(), "an image");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_the_collection_and_its_mods() {
        let files = HashMap::from([(10, ("  Friends ".to_owned(), STELLARIS_APP_ID)), (1, ("Giga".to_owned(), STELLARIS_APP_ID))]);
        let shared = to_shared(10, &[1, 2], &files).unwrap();
        assert_eq!(shared.name, "Friends");
        assert_eq!(shared.mods[0], SharedMod { name: "Giga".into(), steam_id: Some(1), id: "mod/ugc_1.mod".into(), note: None });
        assert_eq!(shared.mods[1].name, "Workshop item 2");

        let other = HashMap::from([(10, ("Skyrim".to_owned(), 489830))]);
        assert!(to_shared(10, &[1], &other).is_err());
    }

    #[test]
    fn reads_update_times() {
        let json = r#"{"response":{"publishedfiledetails":[{"publishedfileid":"1","result":1,"time_updated":1700000000},{"publishedfileid":"2","result":9},{"publishedfileid":"3","result":1}]}}"#;
        assert_eq!(parse_times(json).unwrap(), HashMap::from([(1, 1_700_000_000)]), "missing and unknown items are left out");
    }
}

