# Network proxy DNS validation

The Rama 0.4 connector stack keeps the addresses returned by network-policy validation and passes them through HTTP, CONNECT, and SOCKS5 TCP connection setup. The transport dials these candidates without another DNS lookup. HTTP authority and TLS server identity remain the original host name. A different host or port cannot reuse another destination's checked candidates.

When local networking is explicitly enabled, or a local literal is explicitly allowlisted, the existing opt-in policy still applies. An operator-configured upstream proxy is a separate trusted hop and can resolve its own address independently; that proxy controls its connection to the origin. A failed, empty, or non-public DNS result remains denied when local networking is disabled.
