# Ordnung — project rules

## Always verify feature changes in the running app
After any feature or UI change to the GUI, run `make run` (builds + launches
`ordnung-gui` from source) and confirm it compiles and launches cleanly before
reporting the change as done. Don't stop at `cargo check` — actually run the app.
