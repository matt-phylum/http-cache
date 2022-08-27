# Changelog

## [0.8.0] - 2022-08-26

### Added

- `BadVersion` error type for unknown http versions.

- `BadHeader` error type for bad http header values.

- `new` method for the `HttpResponse` struct.

- `Default` implementation for the `HttpResponse` struct.

- `Action` enum that represents next step actions to be taken as determined by the cache logic.

- `Fetch` enum that represents the type of fetch being performed.

- `Stage` enum that represents a stage of the conditional request process.

- `HttpCache` before_request method which is to be run before the intial request is executed and returns the next action to be taken.

- `HttpCache` after_remote_fetch method which is to be run after a remote fetch action is executed.

- `HttpCache` before_conditional_fetch method which is to be run before a conditional fetch action is executed and returns the next stage of the process.

- `HttpCache` after_conditional_fetch method which is to be run after a conditional fetch action is executed.

### Removed

- `CacheError` enum.

- `Middleware` trait.

- `HttpCache` run method.

- The following dependencies:
  - thiserror
  - miette

### Changed

- `CacheError` enum has been replaced in function by `Box<dyn std::error::Error + Send + Sync>`.

- `Result` typedef is now `std::result::Result<T, BoxError>`.

- `Error` type for the TryFrom implentation for the `HttpVersion` struct is now `BoxError` containing a `BadVersion` error.

- `CacheManager` trait `put` method now returns `Result<(), CacheError>`.

- Updated the minimum versions of the following dependencies:
  - anyhow [1.0.62]
  - async-trait [0.1.57]
  - moka [0.9.3]
  - serde [1.0.144]

## [0.7.0] - 2022-06-17

### Changed

- The `CacheManager` trait is now implemented directly against the `MokaManager` struct rather than `Arc<MokaManager>`. The Arc is now internal to the `MokaManager` struct as part of the `cache` field.

- Updated the minimum versions of the following dependencies:
  - async-trait [0.1.56]
  - http [0.2.8]
  - miette [4.7.1]
  - moka [0.8.5]
  - serde [1.0.137]
  - thiserror [1.0.31]

## [0.6.5] - 2022-04-30

### Changed

- Updated the minimum versions of the following dependencies:
  - http [0.2.7]

## [0.6.4] - 2022-04-26

### Added

- This changelog to keep a record of notable changes to the project.
