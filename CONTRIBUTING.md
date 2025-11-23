# Contributing to ndn-operator

Thanks for helping improve **ndn-operator**! This document highlights the workflow and the patterns that keep the project consistent, with special focus on the controller scaffold macro introduced in `src/macros/controller.rs`.

## Project Workflow

- **Toolchain**: Rust 1.90+ with the 2024 edition. Run `rustup update` before pushing significant changes.
- **Code style**: Always run `cargo fmt` and `cargo clippy --workspace` locally. Our CI expects formatted code.
- **Testing**: Use `cargo test --workspace` plus any scenario-specific commands (e.g., integration tests under `examples/`).
- **Commits**: Keep them small and focused. Reference issues when possible. Commit messages should follow the form `component: short description`.

## Controller Scaffold Macro

All Kubernetes controllers now share the same boilerplate through the `controller_scaffold!` macro (see `src/macros/controller.rs`). This ensures consistent diagnostics, logging, error policies, and graceful shutdown logic across controllers.

### Macro Parameters

```
controller_scaffold! {
    controller_ty: <resource type>,
    reporter: <diagnostics reporter string>,
    run_fn: <public run function>,
    reconcile_fn: <async fn that handles reconciliation>,
    error_policy_fn: <fn name generated in this module>,
    error_requeue_secs: <u64 or expression>,
    api_builder: |client: kube::Client| -> kube::Api<...>,
    watcher_config: <expr returning watcher::Config>,
    preflight: |api: kube::Api<_>| async move { ... } // optional
}
```

- **`controller_ty`**: Usually `super::Foo` or a fully-qualified type. Determines the generated `Context`, `State`, and `Diagnostics` structs.
- **`reporter`**: Human-readable identifier for `Recorder`. Sticks to the `<name>-controller` pattern.
- **`run_fn`**: The exported async function callers invoke. The macro declares it and wires up `Controller::run`.
- **`reconcile_fn`**: Existing reconcile function that accepts `Arc<Resource>` and `Arc<Context>`.
- **`error_policy_fn`**: The macro defines this function; provide a unique identifier. It requeues using `error_requeue_secs`.
- **`error_requeue_secs`**: Either a literal (e.g., `60`) or expression (`5 * 60`).
- **`api_builder`**: Closure returning the `Api<_>` instance used by the controller. Receive a cloned `Client` from the macro.
- **`watcher_config`**: Expression (often `kube::runtime::watcher::Config::default().any_semantic()`) that configures the watcher. For selectors, wrap the expression in `{ ... }` so the macro sees a single token tree.
- **`preflight`** (optional): Closure taking an owned `Api<_>` and returning an async block. Use this for CRD readiness checks; the macro awaits it before starting the controller.

### Generated Items

Each invocation emits:

- `pub struct Context { client, recorder, diagnostics }`
- `#[derive(Clone, Default)] pub struct State`
- `#[derive(Clone, Serialize)] pub struct Diagnostics`
- `fn <error_policy_fn>(...) -> Action`
- `pub async fn <run_fn>(state: State)`

These mirror the old hand-written definitions, so existing code (metrics endpoints, tests) should continue to work without changes.

### Adding a New Controller

1. Create a module under `src/<name>_controller/` with the resource type(s) and reconcile logic.
2. Define the `reconcile_<resource>` function(s) that operate on `Arc<Resource>` + `Arc<Context>` and return `Result<Action>`.
3. Invoke `controller_scaffold!` in `main.rs` of that controller module, configuring `watcher_config` and `preflight` as needed.
4. Export `run_<controller>` and `State` from the module (the macro declares them for you).
5. Wire the new controller into the CLI (`src/main.rs`) following the pattern for existing controllers.
6. Run `cargo fmt` and `cargo check`.

## Pull Requests

- Open a PR early if you want feedback. Drafts are welcome.
- Keep PR descriptions concrete: what changed, why, testing evidence.
- Ensure CI passes before requesting review.

Thanks for contributing! If anything in this document is unclear, feel free to open an issue or start a discussion.
