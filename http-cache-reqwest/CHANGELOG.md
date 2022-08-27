# Changelog

## [0.6.0] - 2022-08-26

### Removed

- The following dependencies:
  - http-cache-semantics (this remains a dependency of http-cache)

### Changed

- Updated the minimum versions of the following dependencies:
  - http-cache [0.8.0]
  - anyhow [1.0.62]
  - async-trait [0.1.57]
  - serde [1.0.144]
  - tokio [1.20.1]

## [0.5.0] - 2022-06-17

### Changed

- The `CacheManager` trait is now implemented directly against the `MokaManager` struct rather than `Arc<MokaManager>`. The Arc is now internal to the `MokaManager` struct as part of the `cache` field.

- Updated the minimum versions of the following dependencies:
  - http-cache [0.7.0]
  - async-trait [0.1.56]
  - http [0.2.8]
  - reqwest [0.11.11]
  - serde [1.0.137]
  - tokio [1.19.2]

## [0.4.5] - 2022-04-30

### Changed

- Updated the minimum versions of the following dependencies:
  - http-cache [0.6.5]
  - http [0.2.7]
  - tokio [1.18.0]

## [0.4.4] - 2022-04-26

### Added

- This changelog to keep a record of notable changes to the project.
