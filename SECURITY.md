# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for a security vulnerability. Instead,
report it privately, either through your code host's private vulnerability
reporting (e.g. GitHub "Report a vulnerability" under the Security tab) or by
email to the maintainer at hola@leotorrealba.com. Include:

- a description of the issue and its impact,
- the smallest reproduction you have (a `.scad` file, an imported mesh, a
  prompt, …),
- the affected version or commit and your platform.

You will get an acknowledgement, and the issue will be handled privately until
a fix or mitigation is released.

## Scope

morphe processes untrusted input: `.scad` files from the internet, imported
meshes and heightmaps, and output from AI models. Issues we especially want
to hear about:

- crashes, hangs or unbounded memory/CPU use triggered by a `.scad` file or
  imported data,
- file reads or writes outside the open project's directory,
- prompt-injection paths that let file content or memory steer the agent
  into actions the user did not approve,
- leaks of the stored API key (logs, error messages, IPC, network requests to
  unintended hosts),
- bypasses of the desktop app's IPC boundary or content security policy.

## Supported versions

morphe is pre-1.0. Security fixes land on the main branch and the latest
release only.
