## Patch Builder

```
Usage:
  patch_builder [OPTIONS] <OLD_DIR> <NEW_DIR> <OUTPUT>
```

**Arguments**

| Name        | Description                                |
|-------------|--------------------------------------------|
| `<OLD_DIR>` | Path to the directory with outdated files  |
| `<NEW_DIR>` | Path to the directory with new files       |
| `<OUTPUT>`  | Path where to create the auto-patcher exe  |

**Options**

| Flag                       | Description                                                                   |
|----------------------------|-------------------------------------------------------------------------------|
| `--product <PRODUCT>`      | Sets the name of the product.                                                 |
| `--from-version <VERSION>` | Sets the semantic version of the version present in `<OLD_DIR>`               |
| `--to-version <VERSION>`   | Sets the semantic version of the version present in `<NEW_DIR>`               |
| `-d, --delete-extra`       | Flag specifying whether additional files in the `<OLD_DIR>` should be deleted |
| `-h, --help`               | Show help                                                                     |


**Examples**

```bash
# Create a patcher for updating 'app_old' to 'app_new', saved as 'updater.exe'
patch_builder app_old app_new updater.exe --product "MyApp" --from_version "1.0" --to_version "1.1"
```