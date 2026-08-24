# Aidoku Novel Sources

Chinese novel sources for [Aidoku](https://github.com/Aidoku/Aidoku), built with [aidoku-rs](https://github.com/Aidoku/aidoku-rs).

| Source         |                 ID |
| -------------- | -----------------: |
| LightNovel.fun | `zh.lightnovelfun` |
| 嗶哩輕小說     |   `zh.twlinovelib` |
| 輕小說文庫     |        `zh.wenku8` |

Aidoku source URL:

```text
https://gholts.github.io/aidoku-source/index.min.json
```

Pushes to `main` that change `sources/**` automatically build and publish this source list.

## Local checks

```sh
cd sources/zh.wenku8
cargo fmt --all --check
cargo clippy
aidoku package
aidoku verify package.aix
```

Run equivalent commands for each source directory.

## License

Licensed under Apache-2.0.

This project has no affiliation with Aidoku or listed content providers.
