{
  description = "Savfox - AI Assistant Gateway & Agent Runtime";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Rust toolchain matching workspace rust-version = "1.89"
        rustToolchain = pkgs.rust-bin.stable."1.89.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # Common build inputs shared between the package and devShell
        darwinDeps = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.apple_sdk.frameworks.Security
          pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          pkgs.libiconv
        ];
      in
      {
        # ── Package: builds the savfox binary ─────────────────────────────
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "savfox";
          version = "0.3.0";
          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
          ];

          buildInputs = with pkgs; [
            openssl
          ] ++ darwinDeps;

          # Only build the CLI binary
          cargoBuildFlags = [ "--bin" "savfox" ];

          # Skip tests during build (run via `nix flake check` instead)
          doCheck = false;

          meta = with pkgs.lib; {
            description = "Savfox AI gateway and agent runtime";
            homepage = "https://github.com/chrislearn/savfox";
            license = with licenses; [ mit asl20 ];
            mainProgram = "savfox";
          };
        };

        # ── DevShell: full development environment ────────────────────────
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust
            rustToolchain
            pkg-config
            openssl

            # Development tools
            cargo-watch
            cargo-nextest
            cargo-llvm-cov

            # Linting and formatting
            typos

            # Docker/container testing
            docker-compose

            # Dioxus CLI for web UI builds
            # (uncomment when dioxus-cli is packaged in nixpkgs)
            # dioxus-cli
          ] ++ darwinDeps;

          shellHook = ''
            echo "Savfox development shell"
            echo "  Rust:    $(rustc --version)"
            echo "  Cargo:   $(cargo --version)"
            echo "  Target:  ${system}"
          '';

          # Environment variables for OpenSSL discovery
          OPENSSL_DIR = "${pkgs.openssl.dev}";
          OPENSSL_LIB_DIR = "${pkgs.openssl.out}/lib";
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
        };

        # ── NixOS Module: systemd service for the gateway ─────────────────
        nixosModules.default = { config, lib, pkgs, ... }:
          let
            cfg = config.services.savfox-gateway;
          in
          {
            options.services.savfox-gateway = {
              enable = lib.mkEnableOption "Savfox Gateway Server";

              package = lib.mkOption {
                type = lib.types.package;
                default = self.packages.${system}.default;
                description = "The savfox package to use.";
              };

              port = lib.mkOption {
                type = lib.types.port;
                default = 18881;
                description = "Port for the gateway HTTP/WebSocket server.";
              };

              host = lib.mkOption {
                type = lib.types.str;
                default = "0.0.0.0";
                description = "Address to bind the gateway server to.";
              };

              token = lib.mkOption {
                type = lib.types.str;
                default = "";
                description = "Authentication token. Leave empty to auto-generate on first start.";
              };

              logLevel = lib.mkOption {
                type = lib.types.str;
                default = "info";
                description = "RUST_LOG filter level (e.g. info, debug, warn).";
              };

              dataDir = lib.mkOption {
                type = lib.types.path;
                default = "/var/lib/savfox";
                description = "Directory for persistent data (sessions, cron, memory).";
              };

              environmentFile = lib.mkOption {
                type = lib.types.nullOr lib.types.path;
                default = null;
                description = ''
                  Path to an environment file containing API keys.
                  Example contents:
                    OPENAI_API_KEY=sk-...
                    ANTHROPIC_API_KEY=sk-ant-...
                '';
              };
            };

            config = lib.mkIf cfg.enable {
              systemd.services.savfox-gateway = {
                description = "Savfox Gateway Server";
                wantedBy = [ "multi-user.target" ];
                after = [ "network-online.target" ];
                wants = [ "network-online.target" ];

                serviceConfig = {
                  ExecStart = lib.concatStringsSep " " ([
                    "${cfg.package}/bin/savfox"
                    "gateway"
                    "--port" (toString cfg.port)
                    "--host" cfg.host
                  ] ++ lib.optionals (cfg.token != "") [
                    "--token" cfg.token
                  ]);

                  Restart = "on-failure";
                  RestartSec = 5;

                  # Sandboxing
                  DynamicUser = true;
                  StateDirectory = "savfox";
                  NoNewPrivileges = true;
                  ProtectSystem = "strict";
                  ProtectHome = true;
                  PrivateTmp = true;
                  ProtectKernelTunables = true;
                  ProtectKernelModules = true;
                  ProtectControlGroups = true;
                  RestrictRealtime = true;
                  RestrictSUIDSGID = true;

                  ReadWritePaths = [ cfg.dataDir ];

                  Environment = [
                    "SAVFOX_HOME=${cfg.dataDir}"
                    "RUST_LOG=${cfg.logLevel}"
                  ];
                } // lib.optionalAttrs (cfg.environmentFile != null) {
                  EnvironmentFile = cfg.environmentFile;
                };
              };
            };
          };
      }
    );
}
