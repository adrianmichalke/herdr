mod catalog;
mod parser;
mod stdio;
pub(crate) mod types;

pub(crate) use stdio::run_stdio;

#[cfg(test)]
mod test_support;
