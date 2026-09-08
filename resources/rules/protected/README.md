# resources/rules/protected

Rule files in this directory define the **built-in protected path registry**
(SPEC §18 and INV-006 / INV-007).

Built-in permanent protection already mandated by the spec:

```text
%USERPROFILE%\.ssh
%USERPROFILE%\.gnupg
%USERPROFILE%\.aws
%USERPROFILE%\.azure
%USERPROFILE%\.kube
```

plus the never-clean roots `C:\`, `%USERPROFILE%`, `%SYSTEMROOT%`,
`%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`, `%PROGRAMDATA%`.

Rules in this directory **can never be overridden** by user/community rules
and always take the highest priority (SPEC §14). `Protected` items never enter
the normal cleanup queue (INV-002) and credentials are permanently fail-safe
(INV-007).

Rule files are **not implemented yet** — they arrive with the Rule Engine in
Phase 3. Until then this directory intentionally contains only this README.
