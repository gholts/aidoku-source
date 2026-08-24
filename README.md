<img src="https://github.com/user-attachments/assets/af090bbc-4093-4ec1-bebc-ee1e9e13b3aa" width="100px" align="left">

### `Aidoku source`

Chinese novel sources for [Aidoku](https://github.com/Aidoku/Aidoku), built with [aidoku-rs](https://github.com/Aidoku/aidoku-rs).

### `list`:

| Source         |                 ID |
| -------------- | -----------------: |
| LightNovel.fun | `zh.lightnovelfun` |
| 嗶哩輕小說     |   `zh.twlinovelib` |
| 輕小說文庫     |        `zh.wenku8` |

### `source URL`:

```text
https://gholts.github.io/aidoku-source/index.min.json
```

### `local check`

```sh
cd sources/<source>
cargo fmt --all --check
cargo clippy
aidoku package
aidoku verify package.aix
```

### `license`

Licensed under Apache-2.0.

This project has no affiliation with Aidoku or listed content providers.
