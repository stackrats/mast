# sail-minimal

Disposable Sail-shaped fixture for docker-gated integration tests. Two tiny
alpine services stand in for the Laravel app + redis, with the stock sail
interpolation shape (`${WWWUSER}` etc. with no in-file defaults).

`vendor/bin/sail` is the real `laravel/sail` runtime script (1.x line, MIT
licensed, © Taylor Otwell / Laravel Holdings — vendored verbatim so resolution
tests exercise genuine sail behavior, per ADR-0001).

Tests always COPY this directory into a uniquely-named temp dir
(`mast-it-<nanos>`), so compose project names never collide and a startup
janitor can sweep crash leftovers by name prefix. Never `docker compose up`
this directory in place.
