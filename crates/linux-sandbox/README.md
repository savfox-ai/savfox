# savfox-linux-sandbox

This crate is responsible for producing:

- a `savfox-linux-sandbox` standalone executable for Linux that is bundled with the Node.js version of the Savfox CLI
- a lib crate that exposes the business logic of the executable as `run_main()` so that
  - the `savfox-exec` CLI can check if its arg0 is `savfox-linux-sandbox` and, if so, execute as if it were `savfox-linux-sandbox`
  - this should also be true of the `savfox` multitool CLI
