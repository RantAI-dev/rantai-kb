# The Rust programming language

## Ownership

Every value in Rust has a single owner. When the owner goes out of scope the
value is dropped. Ownership is how Rust guarantees memory safety without a
garbage collector.

## Why there is no garbage collector

Rust enforces memory rules at compile time using the borrow checker. Because
the compiler proves when memory can be freed, there is no runtime garbage
collection and no stop-the-world pause.

## Borrowing

A reference lets you use a value without taking ownership. At any time you may
have either one mutable reference or any number of shared references.
