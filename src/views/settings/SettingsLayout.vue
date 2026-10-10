<script setup lang="ts">
import SettingsNav from "@/components/settings/SettingsNav.vue";
</script>

<template>
  <div class="flex min-w-0 flex-col">
    <SettingsNav />

    <RouterView v-slot="{ Component, route: sectionRoute }">
      <Transition name="tab-fade" mode="out-in">
        <component :is="Component" :key="String(sectionRoute.name)" />
      </Transition>
    </RouterView>
  </div>
</template>
