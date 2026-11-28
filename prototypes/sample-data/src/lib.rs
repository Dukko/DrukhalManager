//! Made-up mods and conflicts for the UI mockups: no files are read.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Framework,
    Gameplay,
    Ships,
    Graphics,
    Patches,
}

impl Category {
    pub const ALL: [Category; 5] = [Category::Framework, Category::Gameplay, Category::Ships, Category::Graphics, Category::Patches];

    pub fn label(self) -> &'static str {
        match self {
            Category::Framework => "Frameworks",
            Category::Gameplay => "Gameplay",
            Category::Ships => "Ships",
            Category::Graphics => "Graphics & UI",
            Category::Patches => "Patches",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mod {
    pub name: &'static str,
    pub author: &'static str,
    pub version: &'static str,
    pub category: Category,
    pub enabled: bool,
    /// Conflicts this mod wins and loses.
    pub wins: u32,
    pub loses: u32,
    pub files: u32,
    pub objects: u32,
    /// Supports the installed game version.
    pub outdated: bool,
}

#[allow(clippy::too_many_arguments)]
const fn m(name: &'static str, author: &'static str, version: &'static str, category: Category, wins: u32, loses: u32, files: u32, objects: u32, outdated: bool) -> Mod {
    Mod { name, author, version, category, enabled: true, wins, loses, files, objects, outdated }
}

pub fn mods() -> Vec<Mod> {
    use Category::*;
    let mut mods = vec![
        m("UI Overhaul Dynamic", "Elowyn", "4.1.*", Framework, 3, 0, 412, 96, false),
        m("Gigastructural Engineering & More", "Kaiser", "4.1.*", Framework, 21, 6, 3_118, 4_870, false),
        m("Unofficial Patch Collection", "Sorrel", "4.0.*", Framework, 2, 11, 88, 140, true),
        m("Guilli's Planet Modifiers", "Guilli", "4.1.*", Gameplay, 9, 4, 702, 1_204, false),
        m("Expanded Stellaris Traditions", "Hessel", "4.1.*", Gameplay, 6, 9, 455, 880, false),
        m("Ethics & Civics Classic", "Marek", "4.1.*", Gameplay, 14, 3, 1_032, 2_317, false),
        m("Ancient Cache of Technologies", "Ostrava", "4.0.*", Gameplay, 4, 12, 610, 1_450, true),
        m("NSC3 – New Ship Classes", "Ashen", "4.1.*", Ships, 18, 2, 2_244, 3_015, false),
        m("Real Space 4.0", "Lyra", "4.1.*", Graphics, 0, 1, 1_890, 62, false),
        m("Dark UI", "Pell", "4.1.*", Graphics, 1, 5, 214, 0, false),
        m("Drukhal merged patch", "DrukhalManager", "4.1.*", Patches, 29, 0, 46, 61, false),
    ];
    mods[9].enabled = false;
    mods
}

/// A line about the mod, as its Workshop page might open.
pub fn about(name: &str) -> &'static str {
    match name {
        "UI Overhaul Dynamic" => "A rebuilt interface that scales to any resolution, and the base many UI mods build on.",
        "Gigastructural Engineering & More" => "Hundreds of megastructures, new crises and end-game threats.",
        "Unofficial Patch Collection" => "Fixes for bugs in the base game's scripts and events.",
        "Guilli's Planet Modifiers" => "New planet features, modifiers and the buildings that use them.",
        "Expanded Stellaris Traditions" => "More tradition trees, and more choice within each.",
        "Ethics & Civics Classic" => "Extra civics and ethics, balanced against the base game's.",
        "Ancient Cache of Technologies" => "Late-game technologies and the relics that unlock them.",
        "NSC3 – New Ship Classes" => "New ship sizes from dreadnoughts to flagships, with their sections.",
        "Real Space 4.0" => "Realistic star systems, skyboxes and lighting.",
        "Dark UI" => "A darker skin for the game's interface.",
        "Drukhal merged patch" => "Made by DrukhalManager: the merges you've saved.",
        _ => "",
    }
}

/// Mods it needs loaded first, by name.
pub fn needs(name: &str) -> &'static [&'static str] {
    match name {
        "NSC3 – New Ship Classes" | "Gigastructural Engineering & More" => &["UI Overhaul Dynamic"],
        "Dark UI" => &["UI Overhaul Dynamic"],
        _ => &[],
    }
}

#[derive(Debug, Clone)]
pub struct Version {
    pub mod_name: &'static str,
    pub file: &'static str,
    pub text: &'static str,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub category: &'static str,
    pub key: &'static str,
    /// The versions in load order; `winner` is the one the game uses.
    pub versions: Vec<Version>,
    pub winner: usize,
    pub why: &'static str,
    /// The scan found it needs no AI to merge.
    pub simple: Option<&'static str>,
}

pub fn conflicts() -> Vec<Conflict> {
    vec![
        Conflict {
            category: "common/buildings",
            key: "building_research_lab_1",
            versions: vec![
                Version {
                    mod_name: "Guilli's Planet Modifiers",
                    file: "common/buildings/gpm_research.txt",
                    text: "building_research_lab_1 = {\n\tbase_buildtime = 360\n\tcategory = research\n\tresources = {\n\t\tcost = { minerals = 300 }\n\t\tupkeep = { energy = 2 }\n\t\tproduces = {\n\t\t\tphysics_research = 4\n\t\t\tsociety_research = 4\n\t\t}\n\t}\n\ttriggered_planet_modifier = {\n\t\tpotential = { has_modifier = gpm_crystal_caves }\n\t\tmodifier = { planet_researchers_produces_mult = 0.10 }\n\t}\n}",
                },
                Version {
                    mod_name: "Ethics & Civics Classic",
                    file: "common/buildings/ecc_labs.txt",
                    text: "building_research_lab_1 = {\n\tbase_buildtime = 360\n\tcategory = research\n\tresources = {\n\t\tcost = { minerals = 250 }\n\t\tupkeep = { energy = 2 }\n\t\tproduces = {\n\t\t\tphysics_research = 4\n\t\t\tsociety_research = 4\n\t\t\tengineering_research = 2\n\t\t}\n\t}\n\tcountry_modifier = {\n\t\tpotential = { has_civic = civic_technocracy }\n\t}\n}",
                },
            ],
            winner: 1,
            why: "its file name sorts last, and in this folder later files override earlier ones",
            simple: None,
        },
        Conflict {
            category: "events",
            key: "ancrel.1004",
            versions: vec![
                Version { mod_name: "Ancient Cache of Technologies", file: "events/act_relics.txt", text: "country_event = {\n\tid = ancrel.1004\n\ttitle = ancrel.1004.name\n\tis_triggered_only = yes\n}" },
                Version { mod_name: "Unofficial Patch Collection", file: "events/upc_ancrel.txt", text: "country_event = {\n\tid = ancrel.1004\n\ttitle = ancrel.1004.name\n\tis_triggered_only = yes\n\tmean_time_to_happen = { days = 30 }\n}" },
            ],
            winner: 0,
            why: "its file name sorts first, and this folder keeps the first definition it reads",
            simple: None,
        },
        Conflict {
            category: "common/technology",
            key: "tech_mega_engineering",
            versions: vec![
                Version { mod_name: "Gigastructural Engineering & More", file: "common/technology/giga_tech.txt", text: "tech_mega_engineering = {\n\tcost = @tier5cost1\n\tarea = engineering\n\ttier = 5\n}" },
                Version { mod_name: "Ancient Cache of Technologies", file: "common/technology/zz_act_tech.txt", text: "tech_mega_engineering = {\n\tcost = @tier5cost1\n\tarea = engineering\n\ttier = 5\n}" },
            ],
            winner: 1,
            why: "its file name sorts last, and in this folder later files override earlier ones",
            simple: Some("Every mod ships the same version"),
        },
        Conflict {
            category: "common/ship_sizes",
            key: "battleship",
            versions: vec![
                Version { mod_name: "NSC3 – New Ship Classes", file: "common/ship_sizes/nsc_sizes.txt", text: "battleship = {\n\tmax_speed = 120\n\tmax_hitpoints = 3200\n}" },
                Version { mod_name: "Gigastructural Engineering & More", file: "common/ship_sizes/giga_sizes.txt", text: "battleship = {\n\tmax_speed = 100\n\tmax_hitpoints = 3000\n}" },
            ],
            winner: 0,
            why: "its file has the same name as another mod's, and this mod loads later",
            simple: None,
        },
        Conflict {
            category: "localisation",
            key: "l_english:tech_mega_engineering",
            versions: vec![
                Version { mod_name: "Gigastructural Engineering & More", file: "localisation/english/giga_l_english.yml", text: " tech_mega_engineering:0 \"Mega-Engineering\"" },
                Version { mod_name: "Unofficial Patch Collection", file: "localisation/english/upc_l_english.yml", text: " tech_mega_engineering:0 \"Mega Engineering\"" },
            ],
            winner: 0,
            why: "it's in a localisation replace folder, which overrides the others",
            simple: None,
        },
    ]
}
