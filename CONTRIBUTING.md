# Contributing to Recon

Thank you for your interest in contributing to Recon! This document provides guidelines and instructions for contributing to the project.

## Getting Started

### Development Setup

Follow the setup instructions in the [README](README.md#development) to get your development environment ready. In summary, you'll need:

1. **Xcode Command Line Tools**: `xcode-select --install`
2. **Bun**: Install from [bun.sh](https://bun.sh/)
3. **Rust**: Install via [rustup](https://www.rust-lang.org/tools/install)

Clone the repository and install dependencies:

```bash
git clone https://github.com/fylzero/recon.git
cd recon
bun install
```

Run the app in development mode:

```bash
bun run tauri dev
```

## Code Style

### Frontend (Vue/TypeScript)

- Use TypeScript for type safety
- Follow Vue 3 Composition API patterns
- Keep components focused and single-purpose
- Use meaningful variable and function names
- The codebase uses strict TypeScript checking (`strict: true`)

### Backend (Rust)

- Follow Rust standard formatting (the project uses `rustfmt`)
- Write idiomatic Rust code
- Add documentation comments for public APIs
- Handle errors explicitly—avoid unwrapping in production code paths
- Keep functions focused and testable

### General Guidelines

- Write clear, descriptive commit messages
- Keep commits focused on a single logical change
- Add comments only when the code's intent isn't obvious from the code itself

## Running Checks

Before submitting a pull request, ensure your changes pass all checks:

### Type Checking (Frontend)

```bash
bun run build
```

This runs `vue-tsc --noEmit` to check TypeScript types without building.

### Rust Tests

```bash
cd src-tauri
cargo test
```

The Rust backend includes unit tests for core functionality like database operations, filters, SSH connections, and SQL parsing.

### Rust Formatting

```bash
cd src-tauri
cargo fmt --check
```

To automatically format Rust code:

```bash
cd src-tauri
cargo fmt
```

### Rust Linting

```bash
cd src-tauri
cargo clippy -- -D warnings
```

## How to Contribute

### Reporting Bugs

If you find a bug, please [open an issue](https://github.com/fylzero/recon/issues/new/choose) with:

- A clear, descriptive title
- Steps to reproduce the issue
- Expected vs. actual behavior
- Your macOS version and Recon version
- Relevant screenshots or error messages

### Suggesting Features

Feature requests are welcome! Please [open an issue](https://github.com/fylzero/recon/issues/new/choose) with:

- A clear description of the feature
- The problem it solves or use case it addresses
- Any implementation ideas (optional)

### Submitting Pull Requests

1. **Fork the repository** and create a new branch from `main`:
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make your changes** following the code style guidelines above

3. **Test your changes** thoroughly:
   - Run the app with `bun run tauri dev`
   - Test affected functionality manually
   - Run type checking and tests (see "Running Checks" above)

4. **Commit your changes** with clear, descriptive commit messages:
   ```bash
   git commit -m "Add feature: brief description"
   ```

5. **Push to your fork**:
   ```bash
   git push origin feature/your-feature-name
   ```

6. **Open a pull request** against the `main` branch with:
   - A clear title describing the change
   - A description of what changed and why
   - References to any related issues
   - Screenshots for UI changes (if applicable)

### Pull Request Guidelines

- Keep PRs focused on a single feature or fix
- Update documentation if you're changing user-facing behavior
- Make sure all checks pass before requesting review
- Be responsive to feedback and questions
- Don't include unrelated changes or reformatting

## Questions?

If you have questions about contributing, feel free to:

- Open a [discussion](https://github.com/fylzero/recon/discussions)
- Comment on an existing issue
- Reach out to [@fylzero](https://github.com/fylzero)

## License

By contributing to Recon, you agree that your contributions will be licensed under the [MIT License](LICENSE).
