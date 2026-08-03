# Tag fixtures

Verbatim `tags` arrays from `GET registry-1.docker.io/v2/<repo>/tags/list`,
one tag per line, captured 2026-08-03.

They exist so the filter is tested against what registries actually publish —
the patch pins, OS variants and release candidates that make up most of a real
tag list — without a network call in the test suite. Refreshing them is
optional; the filter's job does not change when the tags do, and the tests
assert on shape (no OS variants, moving major leads) rather than on a
particular day's newest release. The two that do name versions
(`mariadb` offering `11.4`, `mysql` topping out at `26`) are pinned
deliberately: they are the cases the old hand-written table got wrong.
