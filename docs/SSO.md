# Single sign-on (OpenID Connect)

Polaris can delegate signing in to an OpenID Connect identity provider such as [Authelia](https://www.authelia.com/), [authentik](https://goauthentik.io/) or [Keycloak](https://www.keycloak.org/). Users then sign in to Polaris with the account they already have at the provider, and do not need a Polaris password.

Signing in with a username and password keeps working alongside single sign-on.

## How it works

1. The user opens `https://your-polaris-server/api/oidc/PROVIDER_NAME/start` in their browser. This is a link you can bookmark, or add as an application to the dashboard of your identity provider.
2. Polaris redirects to the sign-in page of the provider.
3. After signing in, the provider redirects back to Polaris, which opens the web client with the user signed in.

A Polaris user is created the first time someone signs in. Their name is taken from the provider (the `preferred_username` claim by default).

## Configuration

Providers are declared in the Polaris [configuration file](CONFIGURATION.md). They cannot be edited from the web UI.

```toml
# Address browsers use to reach Polaris. This is used to tell the provider where to send users back to.
# When omitted, Polaris derives it from each request (using the X-Forwarded-Proto and X-Forwarded-Host
# headers set by reverse proxies, or the Host header).
public_url = "https://polaris.example.com"

[[oidc_providers]]
# Identifies the provider in URLs. May only contain letters, digits, `-` and `_`. Required.
name = "sso"
# Name of the provider as presented to users.
display_name = "Example SSO"
# Address of the provider. Polaris expects to find the provider's discovery document
# at <issuer>/.well-known/openid-configuration. Required.
issuer = "https://auth.example.com"
# Credentials of the client (application) registered at the provider for Polaris.
# `client_id` is required. Omit `client_secret` for public clients.
client_id = "polaris"
client_secret = "a-long-random-secret"
# Scopes to request in addition to `openid` and `profile`.
scopes = ["groups"]
# Claim to read usernames from. Defaults to "preferred_username".
username_claim = "preferred_username"
# Claim to read roles (or groups) from. Defaults to "groups". Claims nested inside another
# claim are written as a path, eg. "realm_access.roles".
role_claim = "groups"
# When set, only users with at least one of these roles are allowed to sign in.
allowed_roles = ["music"]
# When set, users are Polaris administrators if and only if they have one of these roles.
# This is re-evaluated every time they sign in.
admin_roles = ["polaris-admins"]
# How Polaris authenticates to the provider: "client_secret_basic" (default) or "client_secret_post".
token_auth_method = "client_secret_basic"
# Certificate authority (PEM file) to trust when connecting to the provider, for providers
# whose certificate is not issued by a publicly trusted authority.
ca_file = "/etc/polaris/my-ca.pem"
```

At the provider, register a client for Polaris with:

- **Redirect URI**: `https://your-polaris-server/api/oidc/PROVIDER_NAME/callback`, where `PROVIDER_NAME` is the `name` chosen above.
- **Grant type**: authorization code. Polaris uses PKCE (`S256`), which may be set as required.
- **Client type**: confidential (with a secret) is recommended.

## Security considerations

- **Users are matched by name.** Someone signing in through a provider as `alice` gets access to the existing Polaris user called `alice`, if there is one. Only configure providers you trust to control usernames, and do not let users of the provider pick names that match existing Polaris administrators.
- **Use HTTPS**, both for Polaris and for the provider. Polaris relies on the TLS connection to the provider to authenticate the identity information it receives, and logs a warning for providers whose `issuer` is not an `https://` address.
- Users created through single sign-on are given a random password that is never disclosed. They can only sign in through the provider, unless an administrator sets a password for them. This also means they cannot use [Subsonic clients](SUBSONIC.md) with a password.
- Removing someone's account at the provider prevents new sign-ins, but does not end their existing Polaris sessions. To revoke access immediately, delete the Polaris user or change their password.

## Troubleshooting

The page shown after a failed sign-in is deliberately short on details. The Polaris log contains the reason, including the response of the provider when it rejects a request.

- _"redirect URI mismatch"_ errors at the provider usually mean that Polaris is deriving the wrong address for itself. Set `public_url`, or make sure your reverse proxy forwards the `X-Forwarded-Proto` and `X-Forwarded-Host` headers.
- _"The `preferred_username` claim is missing"_ in the log lists the claims the provider did send. Pick one of them as `username_claim`, or configure the provider to release the missing claim.
- If `allowed_roles` or `admin_roles` do not seem to apply, the log line for the rejected sign-in shows the roles Polaris found in `role_claim`. Many providers only send groups when an additional scope (often `groups`) is requested.
