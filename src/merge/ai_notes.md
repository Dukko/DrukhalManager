# Known changes in Stellaris scripting

- `every_system_colony` → `every_system_planet_colony`, `every_system_ship_colony`: colonies can be planets or ships, so each loop has one form per kind; use both to cover the same colonies.
- `any_system_colony` → `any_system_planet_colony`, `any_system_ship_colony`: same split as every_system_colony.
- `random_system_colony` → `random_system_planet_colony`, `random_system_ship_colony`: same split as every_system_colony.
- `count_system_colony` → `count_system_planet_colony`, `count_system_ship_colony`: same split as every_system_colony.
- `ai_update_type` → removed: technologies no longer have this field; delete the line.
- `include` → `included_buildings`: district specialization (zone) keyword renamed; use included_buildings.
- `exclude` → `excluded_buildings`: district specialization (zone) keyword renamed; use excluded_buildings.
- `defense_armies_add` → `planet_defense_armies_add`: obsolete planet modifier replaced by planet_defense_armies_add.
