# Ordnung — convenience targets. Run `make` to see this list.

.DEFAULT_GOAL := help

.PHONY: help app app-only run genredb-publish

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1mmake %-10s\033[0m %s\n", $$1, $$2}'

app: ## Build, sign, install to /Applications, pin to Dock, relaunch
	@bash tools/build-app.sh

app-only: ## Build + sign the local Ordnung.app, don't touch /Applications
	@bash tools/build-app.sh --no-install

run: ## Run the GUI from source (debug, no bundle, dev features)
	@cargo run -p ordnung-gui --features usb-export

genredb-publish: ## Rebuild the prebuilt genre DB from the Discogs dump (~10 GB stream) and publish it to the rolling `genredb` release. Needs a residential connection; Cloudflare blocks datacenter IPs (see .github/workflows/genredb.yml).
	@cargo run --release -p ordnung-core --example build_genredb -- /tmp/discogs-genres.db
	@gzip -9f /tmp/discogs-genres.db
	@gh release view genredb >/dev/null 2>&1 || gh release create genredb --prerelease \
		--title "Discogs genre database" \
		--notes "Rolling prebuilt genre database, regenerated from the Discogs data dump (CC0). Downloaded automatically by Ordnung's genre-database import."
	@gh release upload genredb /tmp/discogs-genres.db.gz --clobber
	@echo "Published: https://github.com/Kailazy/Ordnung/releases/download/genredb/discogs-genres.db.gz"
