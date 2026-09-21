# Agent-native baseline fixtures

These JSON files are the checked-in, deterministic inputs for the F01
baseline. They are deliberately separate from `docs/examples/`: the examples
describe the proposed public format, while these files are parsed and tested
by the CLI.

Each fixture has `schema_version = 1` and a fixed `component` name. Unknown
fields are rejected. A later scenario runner may add execution metadata, but
it must keep the fixture content hash stable for the same input.

The three baseline components are:

- `Counter`: starts at zero, increments by one, and has an explicit disabled
  state for later interaction variants.
- `LoginForm`: uses a deterministic fixture response and declares only the
  expected `invalid_credentials` business error.
- `VirtualList`: generates 1,000 items with stable keys from `item-0000`
  through `item-0999`, a fixed row height, and disabled animation.

The files are inputs, not runnable applications. Scenario registration and
reset behavior are delivered by the later S01/S02 work packages.
