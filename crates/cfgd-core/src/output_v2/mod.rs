//! New typed-component output system. Ships alongside `output/` during the R1–R3
//! migration; replaces it at R3.
//!
//! See `.claude/specs/2026-05-14-output-system-redesign-design.md` for the design.

pub mod role;
pub use role::Role;

#[cfg(test)]
mod tests {
    #[test]
    fn module_compiles() {
        assert_eq!(2 + 2, 4);
    }
}
