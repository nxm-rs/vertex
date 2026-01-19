{
  description = "Vertex - Ethereum Swarm Node Implementation in Rust";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };

        # Rust stable toolchain with rust-analyzer and WASM target
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-analyzer" "rust-src" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # Get the rust-analyzer binary path for Zed configuration
        rustAnalyzerPath = "${rustToolchain}/bin/rust-analyzer";
      in
      {
        devShells.default = pkgs.mkShell {
          name = "vertex-dev";

          buildInputs = with pkgs; [
            # Rust toolchain
            rustToolchain

            # Build dependencies
            openssl
            openssl.dev
            pkg-config
            gcc
            gnumake

            # libp2p dependencies
            protobuf

            # Development tools
            just               # Task runner
            cargo-audit        # Security audit
            cargo-watch        # File watcher
            cargo-expand       # Macro expansion

            # Git
            git
          ];

          shellHook = ''
            echo "🔷 Vertex Development Environment"
            echo ""
            echo "Rust toolchain: $(rustc --version)"
            echo "rust-analyzer:  $(rust-analyzer --version)"
            echo ""
            echo "Quick commands:"
            echo "  cargo build                    # Build vertex"
            echo "  cargo run -p vertex-node -- node --testnet  # Run testnet node"
            echo "  cargo test                     # Run tests"
            echo "  cargo clippy                   # Lint"
            echo ""

            # Setup .zed directory for Zed editor rust-analyzer integration
            mkdir -p .zed
            ln -sf ${rustAnalyzerPath} .zed/rust-analyzer

            # Create .zed/settings.json if it doesn't exist
            if [ ! -f .zed/settings.json ]; then
              cat > .zed/settings.json << 'EOF'
{
  "lsp": {
    "rust-analyzer": {
      "binary": {
        "path": ".zed/rust-analyzer"
      }
    }
  }
}
EOF
              echo "Created .zed/settings.json for Zed editor"
            fi

            echo "Zed editor: rust-analyzer configured at .zed/rust-analyzer"
            echo ""
          '';

          # OpenSSL for Rust builds
          OPENSSL_DIR = "${pkgs.openssl.dev}";
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";

          # Rust flags for better error messages
          RUST_BACKTRACE = "1";
        };

        # Provide rust-analyzer as a package for tools that need it
        packages = {
          rust-analyzer = rustToolchain;
          default = rustToolchain;
        };
      }
    );
}
