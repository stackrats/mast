<script setup lang="ts" generic="T extends string">
// shadcn-vue's Tabs over reka-ui, restyled to this app's scale and palette.
// Split across Tabs/TabsList/TabsTrigger/TabsContent rather than given a
// closed `options` prop like Select: a tab strip and its panels usually sit in
// different parts of a layout, and the trigger content is markup (counts,
// badges) rather than a label string.
//
// reka-ui supplies the roles, roving focus, and arrow-key navigation, so a tab
// strip is reachable from the keyboard without anyone reimplementing it.
import { TabsRoot } from "reka-ui";

const model = defineModel<T>({ required: true });
</script>

<template>
  <TabsRoot v-model="model">
    <slot />
  </TabsRoot>
</template>
