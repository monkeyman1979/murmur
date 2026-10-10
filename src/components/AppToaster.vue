<script setup lang="ts">
import type { ToasterProps } from "vue-sonner";
import { Toaster } from "vue-sonner";
import "vue-sonner/style.css";

defineProps<{
  theme: NonNullable<ToasterProps["theme"]>;
}>();
</script>

<template>
  <Toaster
    :theme="theme"
    class="murmur-toaster"
    position="bottom-right"
    :duration="4000"
    close-button
    close-button-position="top-right"
  />
</template>

<style>
.murmur-toaster [data-sonner-toast][data-styled="true"] {
  border: 0;
  background: var(--menu);
  color: var(--menu-foreground);
  padding: 0.75rem 2.5rem 0.75rem 0.875rem;
  box-shadow: 0 10px 30px rgb(0 0 0 / 0.18), 0 2px 8px rgb(0 0 0 / 0.1);
}

.murmur-toaster [data-sonner-toast][data-styled="true"] [data-content] {
  min-width: 0;
}

.murmur-toaster [data-sonner-toast][data-styled="true"] [data-title] {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.murmur-toaster [data-sonner-toast][data-styled="true"] [data-close-button] {
  top: 50%;
  right: 0.625rem;
  bottom: auto;
  left: auto;
  border: 0;
  background: transparent;
  color: var(--muted-foreground);
  transform: translateY(-50%);
}

.murmur-toaster [data-sonner-toast][data-styled="true"] [data-close-button]:hover {
  border: 0;
  background: var(--menu-hover);
  color: var(--menu-foreground);
}
</style>
