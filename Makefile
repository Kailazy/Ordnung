# Ordnung — convenience targets. Run `make` to see this list.

.DEFAULT_GOAL := help

.PHONY: help app app-only run

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1mmake %-10s\033[0m %s\n", $$1, $$2}'

app: ## Build, sign, install to /Applications, pin to Dock, relaunch
	@bash tools/build-app.sh

app-only: ## Build + sign the local Ordnung.app, don't touch /Applications
	@bash tools/build-app.sh --no-install

run: ## Run the GUI from source (debug, no bundle)
	@cargo run -p ordnung-gui
