# iCloud authentication: why Calix uses app-specific passwords

Calix authenticates to iCloud with an Apple ID and an app-specific password,
sent as HTTP Basic auth (`caldav.rs`). This note records why, what the
alternative would look like, and what was measured — so the question doesn't
have to be re-researched from scratch next time it comes up.

Investigated 2026-08-10. Everything below marked *measured* was probed directly;
everything marked *inferred* was not, and should be re-verified before anyone
builds on it.

## The alternative: Apple's own token scheme

`caldav.icloud.com` advertises two authentication schemes, not one (*measured*):

```console
$ curl -s -D - -o /dev/null -X PROPFIND https://caldav.icloud.com/ -H "Depth: 0" \
    | grep -i www-authenticate
WWW-Authenticate: Basic realm="MMCalDav"
WWW-Authenticate: X-MobileMe-AuthToken realm="MMCalDav"
```

`X-MobileMe-AuthToken` is what Apple's own devices use. This matters more than
it looks: it means adopting Apple-native auth would be a **credential swap, not
a rewrite**. All of `caldav.rs` — the PROPFIND/REPORT plumbing, ICS parsing,
recurrence handling, the shared generic-CalDAV path — stays exactly as it is.
Only the four `.basic_auth()` call sites change, from
`apple_id:app_password` to `dsid:mmeAuthToken`.

That's worth stating plainly because the obvious assumption is the opposite:
that Apple-native auth implies moving to the private JSON calendar API at
`p*-calendarws.icloud.com`, which pyicloud uses and which *would* mean
rewriting the calendar layer. CalDAV accepting a token means we can skip that
entirely.

## What obtaining that token requires

The flow used by pyicloud, Home Assistant's iCloud integration, and
icloud-photos-downloader (*inferred* — read from those projects, not run here):

| Step | Endpoint | Yields |
| --- | --- | --- |
| 1. SRP-6a login | `idmsa.apple.com/appleauth/auth/signin` | session, 409 challenge |
| 2. 2FA code entry | `/appleauth/auth/verify/trusteddevice/securitycode` | verified session |
| 3. Trust the client | `/appleauth/auth/2sv/trust` | trust token (skips step 2 next time) |
| 4. Exchange | `setup.icloud.com/setup/authenticate` | `dsid` + `mmeAuthToken` |

Endpoint state as probed (*measured*):

| Endpoint | Response | Reading |
| --- | --- | --- |
| `setup.icloud.com/setup/authenticate/` | 400 | alive, wants a well-formed request |
| `idmsa.apple.com/appleauth/auth/signin` | **503** | plain-password signin refused; SRP mandatory |
| `setup.icloud.com/setup/ws/1/accountLogin` | 421 | needs an established session |

Rust crates for the primitives exist: `srp` and `pbkdf2`. Apple's variant is
GSA-SRP — the password is PBKDF2-derived before the exchange — so the textbook
SRP flow does not drop in unmodified. Apple has since added hashcash
proof-of-work headers on top.

## Why we haven't done it

Not feasibility. The decisive reason is that **it would be worse for the
problem it appears to solve.**

| | App-specific password | Apple 2FA / trust token |
| --- | --- | --- |
| Credential lifetime | never expires | ~30 days |
| Routine user action | none | type a 6-digit code monthly |
| Survives Apple protocol changes | yes | no |

An app-specific password is revoked only if you revoke it, or if you change
your Apple ID password (which revokes all of them at once). A trust token
expires on a timer — Home Assistant's users re-authenticate roughly monthly.
Adopting it would convert an occasional annoyance into a scheduled one.

The secondary costs:

- The flow only works if the client sends Apple's own web-client key — a
  calendar app claiming to be iCloud.com. Unsupported by design, and a likely
  terms-of-service violation on the user's primary account.
- Apple actively breaks it. The 503 above is exactly that: every client that
  hadn't implemented SRP stopped working.
- One link is unverified. The `WWW-Authenticate` header proves CalDAV *accepts*
  `X-MobileMe-AuthToken`. It does not prove the SRP flow still *yields* a
  working one. Closing that gap needs a real Apple ID password and a live 2FA
  code, so it was not tested here.

Also worth ruling out explicitly: **Sign in with Apple is not a route to
calendar data.** It authenticates users into your app; it grants no iCloud
service access. The two get conflated constantly.

## When to revisit

- Apple deprecates app-specific passwords for CalDAV.
- Calix wants iCloud data CalDAV can't reach — Reminders, Find My, Photos —
  which needs this stack regardless of the auth question.
- Apple publishes a supported OAuth path for third-party calendar clients.
  [HT121539](https://support.apple.com/en-us/121539) describes Apple-Account
  authorization for *supported* third-party apps, but that is an Apple-side
  allowlist (Outlook is the visible member) with no public client registration.

## Diagnosing "iCloud logged me out"

Usually it hasn't. Before generating a replacement password, prove the stored
one is actually dead:

```sh
PW=$(secret-tool lookup service com.ianswope.Calix \
       username "icloud-app-password:<apple-id>")
curl -s -o /dev/null -w "%{http_code}\n" -X PROPFIND https://caldav.icloud.com/ \
     -u "<apple-id>:$PW" -H "Depth: 0"
```

- **207** — the password is valid. The failure is local: Calix could not *read*
  the secret from the keyring. On Linux this is usually gnome-keyring serving
  stale D-Bus object paths; `systemctl --user restart gnome-keyring-daemon`
  clears it. `app_password()` retries transient keyring errors for this reason.
- **401** — genuinely revoked. Generate a new app-specific password at
  account.apple.com and reconnect.

`caldav.rs` distinguishes these in its error text, so the message tells you
which case you're in rather than leaving "generate a new password" as the
default guess.
