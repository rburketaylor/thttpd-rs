#!/usr/bin/env python3
"""Validate the knowledge system schema and consistency."""

import yaml
import os
import sys

def main():
    knowledge_dir = os.path.join(os.path.dirname(os.path.dirname(__file__)), "knowledge")

    errors = []

    # Load _index.yaml
    index_path = os.path.join(knowledge_dir, "_index.yaml")
    if not os.path.exists(index_path):
        errors.append("_index.yaml not found")
        print(f"FAIL: {errors}")
        sys.exit(1)

    with open(index_path) as f:
        index = yaml.safe_load(f)

    modules = index.get("modules", [])
    print(f"Found {len(modules)} modules in _index.yaml")

    # Check each module has .yaml + .md
    for mod in modules:
        name = mod["name"]
        yaml_path = os.path.join(knowledge_dir, "modules", f"{name}.yaml")
        md_path = os.path.join(knowledge_dir, "modules", f"{name}.md")

        if not os.path.exists(yaml_path):
            errors.append(f"Missing {yaml_path}")
        if not os.path.exists(md_path):
            errors.append(f"Missing {md_path}")

    # Check _migration_map.yaml
    migration_path = os.path.join(knowledge_dir, "_migration_map.yaml")
    if not os.path.exists(migration_path):
        errors.append("_migration_map.yaml not found")
    else:
        with open(migration_path) as f:
            migration = yaml.safe_load(f)
        migrations = migration.get("migrations", [])
        print(f"Found {len(migrations)} migration entries")

        # Verify all modules are covered
        migrated_modules = {m["c_module"].replace(".c", "") for m in migrations}
        indexed_modules = {m["name"] for m in modules}

        missing = indexed_modules - migrated_modules
        if missing:
            errors.append(f"Modules missing from migration map: {missing}")

    if errors:
        for e in errors:
            print(f"ERROR: {e}")
        sys.exit(1)
    else:
        print("PASS: Knowledge system is consistent")
        sys.exit(0)

if __name__ == "__main__":
    main()
