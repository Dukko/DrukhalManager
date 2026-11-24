# Known changes in Stellaris scripting

## Renamed, moved or removed keys

- `every_system_colony` → `every_system_planet_colony`, `every_system_ship_colony`: colonies can be planets or ships, so each loop has one form per kind; use both to cover the same colonies.
- `any_system_colony` → `any_system_planet_colony`, `any_system_ship_colony`: same split as every_system_colony.
- `random_system_colony` → `random_system_planet_colony`, `random_system_ship_colony`: same split as every_system_colony.
- `count_system_colony` → `count_system_planet_colony`, `count_system_ship_colony`: same split as every_system_colony.
- `ai_update_type` → removed: technologies no longer have this field; delete the line.
- `include` → `included_buildings`: district specialization (zone) keyword renamed; use included_buildings.
- `exclude` → `excluded_buildings`: district specialization (zone) keyword renamed; use excluded_buildings.
- `defense_armies_add` → `planet_defense_armies_add`: obsolete planet modifier replaced by planet_defense_armies_add.
- `skill_commander_planet_governor` → `skill_commander_colony_governor`: static modifier renamed; commander leader class defining the old key gets no governor skill effect.
- `skill_official_planet_governor` → `skill_official_colony_governor`: static modifier renamed; official leader class defining the old key gets no governor skill effect.
- `skill_scientist_planet_governor` → `skill_scientist_colony_governor`: static modifier renamed; scientist leader class defining the old key gets no governor skill effect.
- `LEADER_JOB_BACKGROUND_STRATA_WEIGHTS` → removed: use leader_background_weight in the pop category definition instead.
- `leader_background_job_weight` → `leader_background_weight`, `leader_background_class_weight`, `leader_background_ethic_weight`: moved from ethics, civics and leader classes to jobs and pop categories; the old field still loads but is deprecated.
- `pop_ethic_fanatic_authoritarian_attraction_mult` → `pop_ethic_authoritarian_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_egalitarian_attraction_mult` → `pop_ethic_egalitarian_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_xenophobe_attraction_mult` → `pop_ethic_xenophobe_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_xenophile_attraction_mult` → `pop_ethic_xenophile_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_militarist_attraction_mult` → `pop_ethic_militarist_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_pacifist_attraction_mult` → `pop_ethic_pacifist_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_spiritualist_attraction_mult` → `pop_ethic_spiritualist_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `pop_ethic_fanatic_materialist_attraction_mult` → `pop_ethic_materialist_attraction_mult`, `pop_ethic_attraction_mult`: fanatic and absolute ethics have use_for_pops = no, so their pop-attraction modifiers are no longer generated; target the regular ethic or all ethics.
- `every_owned_pop` → `every_owned_pop_group`: iterate pop groups, not individual pops.
- `random_owned_pop` → `random_owned_pop_group`: select a random pop group, not individual pops.
- `any_owned_pop` → `any_owned_pop_group`: check any pop group, not individual pops.
- `count_owned_pop` → `count_owned_pop_group`: count pop groups, not individual pops.
- `ordered_owned_pop` → `ordered_owned_pop_group`: order pop groups, not individual pops.
- `kill_pop` → `kill_pop_group`: kill an amount or percentage of a pop group.
- `create_pop` → `create_pop_group`: create a pop group.

## Behaviour changes that alter old logic

* `is_waystation` is read from starbase levels (`common/starbase_levels`), not starbase types. Move `is_waystation = yes` from the type to the level; the old type placement is ignored. The one-waystation-per-system limit is now enforced in code.
* `triggered_country_modifier` in megastructure types is evaluated with the megastructure as its scope, not the owner country. Wrap country checks in `owner = { ... }`.
* `count_owned_workforce` now includes bonus workforce; counts are higher than before 4.6.
* `add_building = <key>` now picks the zone with a free slot and the highest `ai_priority`; capital buildings go to the government zone; queued constructions and zones replaced by queued swaps are skipped; if no zone has room it adds one district; the block form falls back to `<key>` and logs; a district/zone that cannot hold the building logs an error; a negative `ai_priority` logs an error and counts as 0. Accepts optional `district` and `zone`.
* Docked hull/armor repair speed is now hull/armor regen speed and multiplies regeneration instead of adding a flat amount; old `ship_hull_regen_*` and `ship_armor_regen_*` values may need rebalancing.
* Pop faction `name`, `desc`, `icon` and `visual_ethic` now live in `swappable_data.default`; move old top-level values there.

## Pop Group Rework Patterns

To iterate and kill pops in a colony (replacing legacy `every_owned_pop = { kill_pop = yes }`), use:
```stellaris
every_owned_pop_group = {
	kill_pop_group = {
		pop_group = this
		percentage = 1
	}
}
```

## Other 4.6 additions

| Area | New in 4.6 |
| --- | --- |
| Effects | `clear_variables`, `remove_system`, `set_custom_description`, `transfer_pop_amount keep_automodding`, `delete_ship`/`destroy_ship` block with `kill_leader`, `activate_saved_leader in_training`, `add_random_research_option research_instantly/generate_name_from_effect`, `create_species sapient = this` |
| Triggers | `has_faction_with_ethic`, `ideologies_enabled`; fleet scope for `used_stockpile_capacity_percentage` and `resource_stockpile_compare` |
| Modifiers | `pop_ethic_attraction_add`, `pop_ethic_attraction_mult`, `colony_empire_size_mult`, `leader_lifespan_min`, `guarantee_rare_or_dangerous_tech_add`, `heir_skill_add`, `leader_species_weight_mult`, `country_<class>_pool_size_add`, `country_<class>_external_pool_size_add`, `ship_repair_shield_mult` |
| Leader generation | `leader_background_weight`, `leader_background_class_weight`, `leader_background_ethic_weight` on pop categories and jobs; `leader_background_stratum_weight` on ethics/civics/leader classes; `valid_for_leader_pool` |
| Data | `triggered_building_sets`, `triggered_buildings`, `is_enclave_councilor`, `is_target_ideology`, pop-category swaps and the `pop_cat_` loca prefix, `swappable_data` for pop factions |
| Script values | `polarity`, `max_decimals`, `display_level`; decision enactment time is now a script value |
| Megastructures | `modules`, host-planet modifiers, and a `fromfrom` megastructure scope in `on_dismantle_complete` |
| Triggered modifiers | `should_mult_desc`, `show_potential_if_true`, `mult`; `triggered_planet_modifier` on councilor positions; triggered planet/country modifiers for civics and origins |
| Other | variables can use targets in names (`variable@this`); timed flag effects accept `random`; `add_building` accepts optional `district`/`zone`; seven AI market defines; eight leader-post game rules |
