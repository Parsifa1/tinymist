#import "mod.typ": *

#show: book-page.with(title: "Tinymist LSP")

== Architecture

Tinymist binary has multiple modes, and it may runs multiple actors in background. The actors could run as an async task, in a single thread, or in an isolated process.

The main process of tinymist runs the program as a language server, through stdin and stdout. A main process will fork:
- rendering actors to provide PDF export with watching.
- preview actors that give a document/outline preview over some typst source file.
- compiler actors to provide language APIs.

From the directory structure of `crates/tinymist`, the `main.rs` file parses the command line arguments and starts commands:

```rust
match args.command.unwrap_or_default() {
    Commands::Lsp(args) => lsp_main(args),
    Commands::Compile(args) => compiler_main(args),
    Commands::Preview(args) => tokio_runtime.block_on(preview_main(args)),
    Commands::Probe => Ok(()),
}
```

There are three servers in the `server` directory:
- `lsp.rs` provides the language server, initialized by `lsp_init.rs` and owns commands in `lsp_cmd.rs`.
- `compiler.rs` provides the compiler server, initialized by `compiler_init.rs` and owns commands in `compiler_cmd.rs`.
- `preview.rs` provides a `typst-preview` compatible preview server.

The long-running servers are contributed by the modules in the `server` directory.

They will bootstrap actors in the `actor` directory.

They can construct and return resources in the `resource` directory.

They may invoke tools in the `tool` directory.

== Debugging with input mirroring

You can record the input during running the editors with Tinymist. You can then replay the input to debug the language server.

```sh
# Record the input
tinymist lsp --mirror input.txt
# Replay the input
tinymist lsp --replay input.txt
```

== Analyze memory usage with DHAT

You can build the program with `dhat-heap` feature to collect memory usage with DHAT. The DHAT will instrument the allocator dynamically, so it will slow down the program significantly.

```sh
cargo build --release --bin tinymist --features dhat-heap
```

The instrumented program is nothing different from the normal program, so you can mine the specific memory usage with a lsp session (recorded with `--mirror`) by replaying the input.

```sh
./target/release/tinymist lsp --replay input.txt
...
dhat: Total:     740,668,176 bytes in 1,646,987 blocks
dhat: At t-gmax: 264,604,009 bytes in 317,241 blocks
dhat: At t-end:  259,597,420 bytes in 313,588 blocks
dhat: The data has been saved to dhat-heap.json, and is viewable with dhat/dh_view.html
```

Once you have the `dhat-heap.json`, you can visualize the memory usage with #link("https://nnethercote.github.io/dh_view/dh_view.html")[the DHAT viewer].

== Contributing

See #link("https://github.com/Myriad-Dreamin/tinymist/blob/main/CONTRIBUTING.md")[CONTRIBUTING.md].
