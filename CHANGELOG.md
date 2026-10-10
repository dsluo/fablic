# Changelog

Release Please maintains this file from Conventional Commits when preparing a
release PR.

## 0.1.0 (2026-10-10)


### ⚠ BREAKING CHANGES

* **auth:** login = "entra"/"device" profiles are no longer supported.

### Features

* add az-backed profiles, Fabric client, and workspace folders ([07eec1a](https://github.com/dsluo/fablic/commit/07eec1a1acbb5102b21043e2c52701293225f427))
* **auth:** a profile for the AZURE_CLIENT_* service principal ([222aa9e](https://github.com/dsluo/fablic/commit/222aa9ec995c124a6af2e9a48b0a4a5a3edefa8e))
* **auth:** device-code sign-in with refresh tokens in the OS keychain ([e4a3c0a](https://github.com/dsluo/fablic/commit/e4a3c0a34ae3ad7353bfbb8adc39dafc029efd00))
* **auth:** fablic login works without a config file ([fb6ac12](https://github.com/dsluo/fablic/commit/fb6ac12c55d6deccd3d078e8f8d446b6a4726c1d))
* **auth:** sign in through the browser by default ([725e395](https://github.com/dsluo/fablic/commit/725e3959817030d6f128a18a0b49cbaa2768de52))
* **auth:** treat az service principals as such ([358dd02](https://github.com/dsluo/fablic/commit/358dd023e5d99e01211ba3c714803c616766c8e1))
* **config:** unnamed profiles and bare login/logout use "default" ([a17445d](https://github.com/dsluo/fablic/commit/a17445de1d4e0ae25a9ad4750d2857d8162603d6))
* **fake:** a fuller fake tenant that writes change ([76b7406](https://github.com/dsluo/fablic/commit/76b74068c76501930d7100a322364179d8c3110e))
* **jobs:** add job runs, schedules, and confirmed run/cancel ([6f62678](https://github.com/dsluo/fablic/commit/6f62678047cf12045d8c5531a256180e8b81a8e1))
* **jobs:** reason column and one workspace-wide Spark session lookup ([ee993db](https://github.com/dsluo/fablic/commit/ee993db6ae1b2045693b9fc813353380164bc235))
* **onelake:** browse files and tables, preview, schema, and download ([1c00113](https://github.com/dsluo/fablic/commit/1c001131f9df23f279dfe212ae1426a8b81b60da))
* rename, delete, folders, moves, roles and shortcut writes ([922bcb3](https://github.com/dsluo/fablic/commit/922bcb30186127ea3c8d9491d23eb3870d04f3bc))
* **tui:** add table, command palette, filter, and drill navigation ([4307d0d](https://github.com/dsluo/fablic/commit/4307d0d568e8b70724f88dc7991ef5ebaf5b5e39))
* **tui:** create items, with folders as one of the types ([f41f5b2](https://github.com/dsluo/fablic/commit/f41f5b2c4298866dee52e40a941b96688852cd11))
* **tui:** fixed-width write dialog with a filterable picker for types and roles ([73a30f3](https://github.com/dsluo/fablic/commit/73a30f3a22200eee9f601fa5436e0d2c5ba5aa4f))
* **tui:** mark cut-off cells with a trailing ellipsis ([64356b0](https://github.com/dsluo/fablic/commit/64356b05accb58119a908d2f523d9dfe367a8d7b))
* **tui:** rename profiles, sign in again and see sign-in state in the picker ([07d61d8](https://github.com/dsluo/fablic/commit/07d61d8426dd04d09262ae3c08e176b2d64595ae))
* **tui:** show only the workspace in the header ([5c7f868](https://github.com/dsluo/fablic/commit/5c7f868bfc2f3b2d194b8996165ee13442f4856c))
* **tui:** show the destination folder of a new item in the folder colour ([e77b8b6](https://github.com/dsluo/fablic/commit/e77b8b64d09f6f7bf5ce9c4a565bd034bad335bd))
* **tui:** show the scope path in the bottom breadcrumb ([eb4f996](https://github.com/dsluo/fablic/commit/eb4f996f923062975b8b05eb48a8b858cdd99165))


### Bug Fixes

* **auth:** survive signing in again from the TUI ([17557e8](https://github.com/dsluo/fablic/commit/17557e8bae569b175ce25b0bdbbc1d3f2963f629))
* **jobs:** show user-cancelled notebook and Spark runs as Cancelled ([47a07f1](https://github.com/dsluo/fablic/commit/47a07f1b96372e99590ac010518b8b7dd18ad500))
* **tui:** fit table columns by priority and cut paths from the left ([88bfb0a](https://github.com/dsluo/fablic/commit/88bfb0a66379f8776926b2bc71f9499bf9699e5c))
* **tui:** resume after az login without a cursor query; no panic on narrow terminals ([07f5427](https://github.com/dsluo/fablic/commit/07f5427ab57fd0251da8ed0bf19cb4f6293e7e60))
* **tui:** show the service principal sign-in command in full ([e81bb83](https://github.com/dsluo/fablic/commit/e81bb8379aabc6aff91f19ad4a8648f7355b52fd))
* **tui:** size the profile picker to its contents ([67b7e0d](https://github.com/dsluo/fablic/commit/67b7e0d101e4a686e8bd8c3fb1c2d834523ad40f))


### Code Refactoring

* **auth:** drop fablic's own sign-in; fablic login wraps az login ([195309e](https://github.com/dsluo/fablic/commit/195309e647117f83548223dc8a8a4322bdf93cbf))


### Documentation

* keep tenant data, PII and PHI out of the repo ([b8ccbe6](https://github.com/dsluo/fablic/commit/b8ccbe6c1388ad78aab0fe297fa31ce3ea28ed70))
* update readme with actual human written words ([37cfa40](https://github.com/dsluo/fablic/commit/37cfa40305192eb681b4ce5ae096e99eaf855f69))


### Continuous Integration

* **release:** automate release PRs and binary publishing ([505fb8e](https://github.com/dsluo/fablic/commit/505fb8ece65f71a3e512aea18ad6014ab6835a36))


### Miscellaneous Chores

* **ai:** make CLAUDE.md a symlink to AGENTS.md ([5568b05](https://github.com/dsluo/fablic/commit/5568b058a92a452dd0019d89c79f27844d2aad2a))
* **mise:** load .env into the environment ([2531db4](https://github.com/dsluo/fablic/commit/2531db48ecd112826d37be223fb91f24bc08650d))
* scaffold Rust project with mise tooling ([951cf7f](https://github.com/dsluo/fablic/commit/951cf7f2ff8dd5e00ddf6065f20d7d1c0a6e931c))
