<script setup lang="ts">
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <span class="onboarding-brand-mark shrink-0 text-primary" role="img" :aria-label="t('common.appName')" />
</template>

<style scoped>
.onboarding-brand-mark {
  display: inline-block;
  background-color: currentColor;
  -webkit-mask: url("@/assets/brand/murmur-logo-black.svg") center / contain no-repeat;
  mask: url("@/assets/brand/murmur-logo-black.svg") center / contain no-repeat;
}
</style>
