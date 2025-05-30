// Copyright 2021 The AccessKit Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0 (found in
// the LICENSE-APACHE file) or the MIT license (found in
// the LICENSE-MIT file), at your option.

mod node;
mod util;

mod adapter;
pub use adapter::{Adapter, QueuedEvents};

#[cfg(test)]
mod tests;
