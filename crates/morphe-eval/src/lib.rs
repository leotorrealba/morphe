//! morphe-eval: expression and statement evaluator for Morphe.

pub mod builtins;
pub mod csg;
pub mod eval;
pub mod include;
pub mod print;
pub mod scope;
pub mod stack;
pub mod value;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
