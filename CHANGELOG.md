# Kalatori Changelog

All notable changes to this project will be documented in this file.

## [0.2.8] - 2024-11-13

### 🚀 Features

- Order transaction storage implementation.

## [0.2.7] - 2024-11-18

### 🚀 Features

- Asset Hub transactions with fee currency
- Autofill tip with asset
- Pass asset id into transaction constructor to properly select fee currency

### 🧪 Testing

- Test cases to cover partial withdrawal and Asset Gub transfers

## [0.2.6] - 2024-11-01

### 🚀 Features

- Force withdrawal call implementation
- Docker container for the app
- Containerized test environment

### 🐛 Bug Fixes

- Fixed the storage fetching.
- Removed redundant name checks & thereby fixed the connection to Asset Hub chains.

## [0.2.5] - 2024-10-29

### 🚀 Features

- Callback in case callback url provided

### 🐛 Bug Fixes

- fix error handling as a result of dep uupgrade
- fix order withdraw transaction
- mark order withdrawn on successful withdraw

## [0.2.4] - 2024-10-21

### ⚡ Performance

- Switched from the unmaintained `hex` crate to `const-hex`.

### 🚜 Refactor

- Moved all utility modules under the utils module.
- Removed all `mod.rs` files & added a lint rule to prevent them.

## [0.2.3] - 2024-10-15

### 🚀 Features

- Server health call implementation

## [0.2.2] - 2024-10-10

### 🚀 Features

- Docker environment for chopsticks and compose to spawn 4 chopsticks instances in parallel looking at different RPCs

### 🐛 Bug Fixes

- Server_status API request returns instance_id instead of placeholder
- Mark_paid function will mark order correctly now

## [0.2.1] - 2024-10-07

Making the order request work according to specs in the [specs](https://alzymologist.github.io/kalatori-api/#/).
Using the tests from [kalatori-api-test-suite]() in order to validate.
Added git cliff and configuration for it to generate CHANGELOG like this one, see [CONTRIBUTING.md](CONTRIBUTING.md)

### 🐛 Bug Fixes

- API specs Balances->Native
- Not having currency in the request responds with Fatal
- Validate missing order parameters
- Get order handler functionality part
- Get order and update order
- Removed version check for PRs

### ⚙️ Miscellaneous Tasks

- Resolve conflicts
