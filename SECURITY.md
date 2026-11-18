# Security

Please report vulnerabilities privately through GitHub Security Advisories once the public repository is available. Do not post credentials, private prompts, generated personal data, or exploit details in a public issue.

The harness binds model servers and adapters to loopback, keeps credentials outside the repository, gives workers only configured tools/hosts, limits dispatch and time budgets, and verifies downloaded artifacts before activation. These controls reduce risk; they do not make arbitrary model output trustworthy.

Supported security updates will target the newest tagged release. The alpha source checkout is not a signed distribution.

