# GeissOxide — build, check and package. Run `make help` for the target list (documented in MAKEFILE.md).

NAME      := geissoxide
VERSION   := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
BUNDLE_ID ?= org.geissoxide.visualizer
DIST      := dist
APP       := $(DIST)/GeissOxide.app
UNAME_S   := $(shell uname -s)
ARGS      ?=
PRESETS_URL := https://github.com/projectM-visualizer/presets-milkdrop-original/archive/refs/heads/master.zip
GH_WORKFLOW := release.yml

.DEFAULT_GOAL := all
.PHONY: all build run secu sécu darwin linux dist presets help clean mrproper

all: secu build ## Check (secu) then build a release binary for the current OS/arch

build:
	cargo build --release

run: all ## Build then run the app; pass options with ARGS="--engine milkdrop"
	./target/release/$(NAME) $(ARGS)

secu: ## Security and best-practice checks: fmt, clippy, audit, deny, tests
	cargo fmt --all -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo audit
	cargo deny check
	cargo test

sécu: secu

darwin: ## secu + macOS universal (arm64 + x86_64) .app bundle in dist/
ifneq ($(UNAME_S),Darwin)
	$(error 'make darwin' must run on macOS)
endif
	$(MAKE) secu
	cargo build --release --target aarch64-apple-darwin
	cargo build --release --target x86_64-apple-darwin
	rm -rf "$(APP)"
	mkdir -p "$(APP)/Contents/MacOS" "$(APP)/Contents/Resources"
	lipo -create -output "$(APP)/Contents/MacOS/$(NAME)" \
	    target/aarch64-apple-darwin/release/$(NAME) target/x86_64-apple-darwin/release/$(NAME)
	sed -e 's/@VERSION@/$(VERSION)/g' -e 's/@BUNDLE_ID@/$(BUNDLE_ID)/g' packaging/Info.plist > "$(APP)/Contents/Info.plist"
	iconutil -c icns -o "$(APP)/Contents/Resources/GeissOxide.icns" packaging/icon.iconset
	[ -d presets ] && cp -R presets "$(APP)/Contents/Resources/presets" || true
	codesign --force --deep --sign - "$(APP)"
	lipo -info "$(APP)/Contents/MacOS/$(NAME)"

linux: ## secu + Linux amd64 executable in dist/ (native on Linux; via GitHub Actions + gh on macOS)
ifeq ($(UNAME_S),Linux)
	$(MAKE) secu
	cargo build --release --target x86_64-unknown-linux-gnu
	mkdir -p $(DIST)
	cp target/x86_64-unknown-linux-gnu/release/$(NAME) $(DIST)/$(NAME)-linux-amd64
else
	$(MAKE) secu
	@command -v gh >/dev/null || { echo "gh (GitHub CLI) is required to build Linux binaries from macOS"; exit 1; }
	gh workflow run $(GH_WORKFLOW) --ref "$$(git branch --show-current)"
	sleep 5
	gh run watch "$$(gh run list --workflow $(GH_WORKFLOW) --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status
	mkdir -p $(DIST)
	gh run download "$$(gh run list --workflow $(GH_WORKFLOW) --limit 1 --json databaseId --jq '.[0].databaseId')" \
	    --name $(NAME)-linux-amd64 --dir $(DIST)
	chmod +x $(DIST)/$(NAME)-linux-amd64
endif

dist: darwin linux ## Build every distributable (macOS .app + Linux executable)

presets: ## Download the original MilkDrop preset pack into presets/
	rm -rf presets presets.zip
	curl -L -o presets.zip $(PRESETS_URL)
	unzip -q presets.zip
	mv presets-milkdrop-original-master/Milkdrop-Original presets
	rm -rf presets-milkdrop-original-master presets.zip

help: ## Show this help
	@grep -hE '^[a-zA-Zé_-]+:.*## ' $(MAKEFILE_LIST) | sed 's/:.*## /|/' | awk -F'|' '{ printf "  %-10s %s\n", $$1, $$2 }'

clean: ## Remove build artefacts (cargo clean)
	cargo clean

mrproper: clean ## clean + remove distributables and downloaded presets
	rm -rf $(DIST) presets presets.zip
