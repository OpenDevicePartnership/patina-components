# Patina Components

This repository currently serves as an evaluation area for breaking out Patina components to their own repo. Meaning,
initially, only new components will be added to this repo with no change to existing components in the `patina`
repository.

## Rationale

Patina components are ultimately pieces of functionality largely authored by the ecosystem that extend the base set of
functionalities provided by Patina core code while leveraging [Patina SDK code](https://github.com/OpenDevicePartnership/patina/tree/main/sdk).
As today's firmware code is a mix of open and closed source, so will be Patina components.

The primary purpose for this repo split is to better reflect the reality of detached repo component development within
the Patina project itself so shared challenges with downstream code are readily apparent to Patina maintainers.

Because Patina components are maintained as separate crates, the crate boundary makes repo placement less important
from a code sharing perspective, but independent versioning to Patina SDK changes, the ability to individually version
components, etc. better reflect how components will be maintained outside the patina repo.

Based on this evaluation, other components might move to this repository in the future.

## Resources

- [Patina](https://github.com/OpenDevicePartnership/patina)
- [Patina Documentation](https://opendevicepartnership.github.io/patina/)
- [Getting Started with Patina Components](https://opendevicepartnership.github.io/patina/component/getting_started.html)

## Quick Start

### Build

See the [First-Time Tool Setup Instructions](https://github.com/OpenDevicePartnership/patina#first-time-tool-setup-instructions)
in the patina repository for instructions on setting up your development environment.
