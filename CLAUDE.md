

## All code you write MUST be fully optimized.

"Fully optimized" includes:

- maximizing algorithmic big-O efficiency for memory and runtime
- using parallelization and SIMD where appropriate
- following proper style conventions for Rust (e.g. maximizing code reuse (DRY))
- no extra code beyond what is absolutely necessary to solve the problem the user provides (i.e. no technical debt)

## Code Style and Formatting

- MUST use meaningful, descriptive variable and function names
- MUST follow Rust API Guidelines and idiomatic Rust conventions
- MUST use 4 spaces for indentation (never tabs)
- NEVER use emoji, or unicode that emulates emoji (e.g. ✓, ✗). The only exception is when writing tests and testing the impact of multibyte characters.
- Use snake_case for functions/variables/modules, PascalCase for types/traits, SCREAMING_SNAKE_CASE for constants
- Limit line length to 100 characters (rustfmt default)
- Assume the user is a Python expert, but a Rust novice. Include additional code comments around Rust-specific nuances that a Python developer may not recognize.

## Documentation

- MUST include doc comments for all public functions, structs, enums, and methods
- MUST document function parameters, return values, and errors
- Keep comments up-to-date with code changes
- Include examples in doc comments for complex functions

## Type System

- MUST leverage Rust's type system to prevent bugs at compile time
- NEVER use .unwrap() in library code; use .expect() only for invariant violations with a descriptive message
- MUST use meaningful custom error types with thiserror
- Use newtypes to distinguish semantically different values of the same underlying type
- Prefer Option<T> over sentinel values

## Error Handling 

- NEVER use .unwrap() in production code paths
- MUST use Result<T, E> for fallible operations
- MUST use thiserror for defining error types and anyhow for application-level errors
- MUST propagate errors with ? operator where appropriate
- Provide meaningful error messages with context using .context() from anyhow

## Function Design

- MUST keep functions focused on a single responsibility
- MUST prefer borrowing (&T, &mut T) over ownership when possible
- Limit function parameters to 5 or fewer; use a config struct for more
- Return early to reduce nesting
- Use iterators and combinators over explicit loops where clearer

Remember: Prioritize clarity and maintainability over cleverness.
