<script setup lang="ts">
// The MAST wordmark, as vectors.
//
// It was six rows of box-drawing characters in a `<pre>` at `text-[3px]`, for
// the same reason the mark once was: the CLI banner draws it that way and the
// two should introduce the app identically. At 3px that depends on the
// rasteriser keeping a 5-unit rule distinguishable inside a 65-unit cell scaled
// to three pixels, which it cannot — rows merge or vanish depending on how the
// size rounds. Zoom changes the rounding, so the wordmark appeared, broke, and
// disappeared at different zoom levels, and was mush at 100%.
//
// Generated from the same art on the same 30x65 grid the mark's paths use, so
// the two halves still share one grid and still line up row for row. The last
// row is rules rather than blocks, so the art fills 365 of 6x65 units — the
// mark's viewBox height exactly, which is what keeps `items-start` aligning
// them.
//
// `currentColor` so callers set it with text color, like any glyph, and
// `aria-hidden` because the menubar's wrapper already carries role="img" and
// the label — two nested image roles announce the app's name twice.
</script>

<template>
  <svg
    viewBox="0 0 1170 365"
    fill="currentColor"
    shape-rendering="crispEdges"
    aria-hidden="true"
    focusable="false"
  >
    <rect x="0" y="0" width="90" height="65" />
    <rect x="210" y="0" width="90" height="65" />
    <rect x="390" y="0" width="150" height="65" />
    <rect x="630" y="0" width="210" height="65" />
    <rect x="900" y="0" width="240" height="65" />
    <rect x="90" y="25" width="23" height="5" />
    <rect x="300" y="25" width="23" height="5" />
    <rect x="540" y="25" width="23" height="5" />
    <rect x="840" y="25" width="23" height="5" />
    <rect x="1140" y="25" width="23" height="5" />
    <rect x="108" y="25" width="5" height="40" />
    <rect x="318" y="25" width="5" height="40" />
    <rect x="558" y="25" width="5" height="40" />
    <rect x="858" y="25" width="5" height="40" />
    <rect x="1158" y="25" width="5" height="40" />
    <rect x="90" y="35" width="13" height="5" />
    <rect x="300" y="35" width="13" height="5" />
    <rect x="540" y="35" width="13" height="5" />
    <rect x="840" y="35" width="13" height="5" />
    <rect x="1140" y="35" width="13" height="5" />
    <rect x="98" y="35" width="5" height="30" />
    <rect x="308" y="35" width="5" height="30" />
    <rect x="548" y="35" width="5" height="30" />
    <rect x="848" y="35" width="5" height="30" />
    <rect x="1148" y="35" width="5" height="30" />
    <rect x="848" y="65" width="5" height="30" />
    <rect x="918" y="65" width="5" height="30" />
    <rect x="1148" y="65" width="5" height="30" />
    <rect x="858" y="65" width="5" height="40" />
    <rect x="908" y="65" width="5" height="40" />
    <rect x="1158" y="65" width="5" height="40" />
    <rect x="0" y="65" width="120" height="65" />
    <rect x="180" y="65" width="120" height="65" />
    <rect x="308" y="65" width="5" height="65" />
    <rect x="318" y="65" width="5" height="65" />
    <rect x="360" y="65" width="60" height="65" />
    <rect x="510" y="65" width="60" height="65" />
    <rect x="630" y="65" width="60" height="65" />
    <rect x="990" y="65" width="60" height="65" />
    <rect x="120" y="90" width="23" height="5" />
    <rect x="428" y="90" width="82" height="5" />
    <rect x="570" y="90" width="23" height="5" />
    <rect x="698" y="90" width="155" height="5" />
    <rect x="918" y="90" width="72" height="5" />
    <rect x="1058" y="90" width="95" height="5" />
    <rect x="138" y="90" width="5" height="40" />
    <rect x="428" y="90" width="5" height="40" />
    <rect x="588" y="90" width="5" height="40" />
    <rect x="698" y="90" width="5" height="40" />
    <rect x="1058" y="90" width="5" height="40" />
    <rect x="120" y="100" width="13" height="5" />
    <rect x="438" y="100" width="72" height="5" />
    <rect x="570" y="100" width="13" height="5" />
    <rect x="708" y="100" width="155" height="5" />
    <rect x="908" y="100" width="82" height="5" />
    <rect x="1068" y="100" width="95" height="5" />
    <rect x="128" y="100" width="5" height="30" />
    <rect x="438" y="100" width="5" height="30" />
    <rect x="578" y="100" width="5" height="30" />
    <rect x="708" y="100" width="5" height="30" />
    <rect x="1068" y="100" width="5" height="30" />
    <rect x="0" y="130" width="60" height="65" />
    <rect x="90" y="130" width="120" height="65" />
    <rect x="240" y="130" width="60" height="65" />
    <rect x="308" y="130" width="5" height="65" />
    <rect x="318" y="130" width="5" height="65" />
    <rect x="360" y="130" width="210" height="65" />
    <rect x="578" y="130" width="5" height="65" />
    <rect x="588" y="130" width="5" height="65" />
    <rect x="630" y="130" width="210" height="65" />
    <rect x="990" y="130" width="60" height="65" />
    <rect x="1058" y="130" width="5" height="65" />
    <rect x="1068" y="130" width="5" height="65" />
    <rect x="68" y="155" width="22" height="5" />
    <rect x="218" y="155" width="22" height="5" />
    <rect x="840" y="155" width="23" height="5" />
    <rect x="68" y="155" width="5" height="40" />
    <rect x="218" y="155" width="5" height="40" />
    <rect x="858" y="155" width="5" height="40" />
    <rect x="78" y="165" width="12" height="5" />
    <rect x="228" y="165" width="12" height="5" />
    <rect x="840" y="165" width="13" height="5" />
    <rect x="78" y="165" width="5" height="30" />
    <rect x="228" y="165" width="5" height="30" />
    <rect x="848" y="165" width="5" height="30" />
    <rect x="108" y="195" width="5" height="30" />
    <rect x="218" y="195" width="5" height="30" />
    <rect x="648" y="195" width="5" height="30" />
    <rect x="98" y="195" width="5" height="40" />
    <rect x="228" y="195" width="5" height="40" />
    <rect x="638" y="195" width="5" height="40" />
    <rect x="0" y="195" width="60" height="65" />
    <rect x="68" y="195" width="5" height="65" />
    <rect x="78" y="195" width="5" height="65" />
    <rect x="120" y="195" width="60" height="65" />
    <rect x="240" y="195" width="60" height="65" />
    <rect x="308" y="195" width="5" height="65" />
    <rect x="318" y="195" width="5" height="65" />
    <rect x="360" y="195" width="60" height="65" />
    <rect x="510" y="195" width="60" height="65" />
    <rect x="578" y="195" width="5" height="65" />
    <rect x="588" y="195" width="5" height="65" />
    <rect x="780" y="195" width="60" height="65" />
    <rect x="848" y="195" width="5" height="65" />
    <rect x="858" y="195" width="5" height="65" />
    <rect x="990" y="195" width="60" height="65" />
    <rect x="1058" y="195" width="5" height="65" />
    <rect x="1068" y="195" width="5" height="65" />
    <rect x="108" y="220" width="12" height="5" />
    <rect x="188" y="220" width="35" height="5" />
    <rect x="428" y="220" width="82" height="5" />
    <rect x="648" y="220" width="132" height="5" />
    <rect x="188" y="220" width="5" height="40" />
    <rect x="428" y="220" width="5" height="40" />
    <rect x="98" y="230" width="22" height="5" />
    <rect x="198" y="230" width="35" height="5" />
    <rect x="438" y="230" width="72" height="5" />
    <rect x="638" y="230" width="142" height="5" />
    <rect x="198" y="230" width="5" height="30" />
    <rect x="438" y="230" width="5" height="30" />
    <rect x="138" y="260" width="5" height="30" />
    <rect x="188" y="260" width="5" height="30" />
    <rect x="128" y="260" width="5" height="40" />
    <rect x="198" y="260" width="5" height="40" />
    <rect x="0" y="260" width="60" height="65" />
    <rect x="68" y="260" width="5" height="65" />
    <rect x="78" y="260" width="5" height="65" />
    <rect x="240" y="260" width="60" height="65" />
    <rect x="308" y="260" width="5" height="65" />
    <rect x="318" y="260" width="5" height="65" />
    <rect x="360" y="260" width="60" height="65" />
    <rect x="428" y="260" width="5" height="65" />
    <rect x="438" y="260" width="5" height="65" />
    <rect x="510" y="260" width="60" height="65" />
    <rect x="578" y="260" width="5" height="65" />
    <rect x="588" y="260" width="5" height="65" />
    <rect x="630" y="260" width="210" height="65" />
    <rect x="848" y="260" width="5" height="65" />
    <rect x="858" y="260" width="5" height="65" />
    <rect x="990" y="260" width="60" height="65" />
    <rect x="1058" y="260" width="5" height="65" />
    <rect x="1068" y="260" width="5" height="65" />
    <rect x="138" y="285" width="55" height="5" />
    <rect x="128" y="295" width="75" height="5" />
    <rect x="18" y="325" width="5" height="30" />
    <rect x="68" y="325" width="5" height="30" />
    <rect x="258" y="325" width="5" height="30" />
    <rect x="308" y="325" width="5" height="30" />
    <rect x="378" y="325" width="5" height="30" />
    <rect x="428" y="325" width="5" height="30" />
    <rect x="528" y="325" width="5" height="30" />
    <rect x="578" y="325" width="5" height="30" />
    <rect x="648" y="325" width="5" height="30" />
    <rect x="848" y="325" width="5" height="30" />
    <rect x="1008" y="325" width="5" height="30" />
    <rect x="1058" y="325" width="5" height="30" />
    <rect x="8" y="325" width="5" height="40" />
    <rect x="78" y="325" width="5" height="40" />
    <rect x="248" y="325" width="5" height="40" />
    <rect x="318" y="325" width="5" height="40" />
    <rect x="368" y="325" width="5" height="40" />
    <rect x="438" y="325" width="5" height="40" />
    <rect x="518" y="325" width="5" height="40" />
    <rect x="588" y="325" width="5" height="40" />
    <rect x="638" y="325" width="5" height="40" />
    <rect x="858" y="325" width="5" height="40" />
    <rect x="998" y="325" width="5" height="40" />
    <rect x="1068" y="325" width="5" height="40" />
    <rect x="18" y="350" width="55" height="5" />
    <rect x="258" y="350" width="55" height="5" />
    <rect x="378" y="350" width="55" height="5" />
    <rect x="528" y="350" width="55" height="5" />
    <rect x="648" y="350" width="205" height="5" />
    <rect x="1008" y="350" width="55" height="5" />
    <rect x="8" y="360" width="75" height="5" />
    <rect x="248" y="360" width="75" height="5" />
    <rect x="368" y="360" width="75" height="5" />
    <rect x="518" y="360" width="75" height="5" />
    <rect x="638" y="360" width="225" height="5" />
    <rect x="998" y="360" width="75" height="5" />
  </svg>
</template>
