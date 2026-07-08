# Introduction

[Patina Components](https://github.com/OpenDevicePartnership/patina-components) is a repository that hosts Patina
firmware components that are developed and maintained as independent crates. A Patina component is a unit of firmware
functionality that extends the base capabilities provided by Patina core code. Components build on the
[Patina SDK](https://docs.rs/patina/latest/patina/). This repository is a central place to share open source components
with other Patina platforms.

- For more general information about Patina components, see
  [Getting Started with Patina Components](https://opendevicepartnership.github.io/patina/component/getting_started.html).

``` admonish note
Components in this repo are versioned independently, so they can evolve independently of each other. Each component must
have at least two designated maintainers who will be assigned for reviews of that component in a CODEOWNERS file.
```

## Why this repository exists

Hosting components here brings real-world component development work closer to the Patina core team, so the challenges
downstream authors face, such as reacting to SDK changes and coordinating versions, are visible to Patina maintainers
as they occur. In addition, a centralized repo allows the Patina community to more efficiently share, collaborate, and
apply consistent best practices across components.

As a component author or maintainer, please consider contributing to this repository. Also understand that by using this
repository, you are agreeing to maintain your component here following Patina community standards and best practices.

The Patina maintainer team ultimately reserves the right to remove components that are not maintained or do not meet the
standards of the Patina community. Lack of responses from component maintainers to communication and reviews will result
in the removal of that component.

## What goes in this documentation

Most Patina documentation is hosted in the main [Patina documentation](https://opendevicepartnership.github.io/patina/).
This mdbook only contains information specific to this repo and components in the repo. It does not host general Patina
component architecture, design, and other information that is not specific to this repo.

Also, it is recommended to host most component documentation in the component crate itself, so that it is available to
users of the crate on [docs.rs](https://docs.rs/) and to treat any additional documentation in this mdbook as a
supplement to the crate documentation, if necessary.

## Where to go next

- [Patina](https://github.com/OpenDevicePartnership/patina) for the core project and SDK.
- [Patina Documentation](https://opendevicepartnership.github.io/patina/) for overall Patina project documentation.
