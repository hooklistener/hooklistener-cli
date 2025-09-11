# Contributing to Hooklistener CLI

Thank you for your interest in contributing to Hooklistener CLI! We welcome contributions from the community and are grateful for any help you can provide.

## Code of Conduct

Please note that this project is released with a [Code of Conduct](CODE_OF_CONDUCT.md). By participating in this project you agree to abide by its terms.

## How to Contribute

### Reporting Issues

- Check if the issue has already been reported in the [Issues](https://github.com/hooklistener/hooklistener-cli/issues) section
- If not, create a new issue with:
  - A clear, descriptive title
  - Steps to reproduce the problem
  - Expected vs actual behavior
  - Your environment (OS, Rust version, etc.)
  - Any relevant logs or screenshots

### Suggesting Features

- Open a [Discussion](https://github.com/hooklistener/hooklistener-cli/discussions) first to gauge interest
- For approved features, create an issue with the `enhancement` label
- Provide clear use cases and implementation ideas

### Pull Requests

1. **Fork the repository** and create your branch from `main`
2. **Follow the setup instructions** in the README
3. **Make your changes**:
   - Write clear, concise commit messages
   - Follow the existing code style
   - Add tests for new functionality
   - Update documentation as needed
4. **Test your changes**:
   ```bash
   cargo test --all-targets --all-features --locked
   cargo fmt --all -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   ```
5. **Submit a Pull Request**:
   - Reference any related issues
   - Describe your changes in detail
   - Include screenshots for UI changes

## Development Guidelines

### Code Style

- Follow Rust standard conventions
- Use `cargo fmt` to format your code
- Use `cargo clippy` to catch common mistakes
- Write meaningful variable and function names
- Add comments for complex logic

### Testing

- Write unit tests for new functions
- Add integration tests for new features
- Ensure all tests pass before submitting PR
- Aim for good test coverage

### Documentation

- Update README.md if adding new features
- Add inline documentation for public APIs
- Update CHANGELOG.md for user-facing changes
- Include examples where appropriate

### Commit Messages

Follow conventional commit format:
```
type(scope): description

[optional body]

[optional footer]
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `test`: Test additions or corrections
- `chore`: Maintenance tasks

Example:
```
feat(ui): add search functionality

Implements fuzzy search for webhook requests
with keyboard shortcut (/) and filter persistence.

Closes #42
```

## Getting Help

- Join our [Discussions](https://github.com/hooklistener/hooklistener-cli/discussions)
- Check the [Wiki](https://github.com/hooklistener/hooklistener-cli/wiki)
- Reach out to maintainers in issues

## Recognition

Contributors will be recognized in:
- The project README
- Release notes
- GitHub's contributor graph

Thank you for helping make Hooklistener CLI better!