{
  description = "Chat with Work Local Agent: share folders with Chat with Work, read-only";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          cww = pkgs.rustPlatform.buildRustPackage {
            pname = "cww";
            version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
            src = self;

            # Registry dependencies come straight from Cargo.lock, so a lock
            # file update needs no vendor hash refresh.
            cargoLock.lockFile = ./Cargo.lock;

            # The end-to-end test binds a local port; the rest touches only
            # temporary directories.
            __darwinAllowLocalNetworking = true;

            postInstall = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              install -Dm644 packaging/systemd/cww.service $out/lib/systemd/user/cww.service
              substituteInPlace $out/lib/systemd/user/cww.service \
                --replace-fail /usr/bin/cww $out/bin/cww \
                --replace-fail /usr/bin/mkdir ${pkgs.coreutils}/bin/mkdir
            '';

            meta = {
              description = "Chat with Work Local Agent: share folders with Chat with Work, read-only";
              homepage = "https://github.com/crmne/chatwithwork-local-agent";
              license = with pkgs.lib.licenses; [
                mit
                asl20
              ];
              mainProgram = "cww";
              platforms = pkgs.lib.platforms.linux ++ pkgs.lib.platforms.darwin;
            };
          };
        in
        {
          default = cww;
          inherit cww;
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.cww ];
          packages = with pkgs; [
            rust-analyzer
            clippy
            rustfmt
            cargo-deny
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
