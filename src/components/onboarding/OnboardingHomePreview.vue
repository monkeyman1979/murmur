<script setup lang="ts">
import homePreview from "@/assets/onboarding/home-preview.jpg";
import { useI18n } from "vue-i18n";

const { t, locale } = useI18n();
</script>

<template>
  <figure class="relative min-w-0" data-testid="home-product-preview">
    <div class="home-preview-crop overflow-hidden rounded-tl-3xl border-t border-l border-border bg-card">
      <img
        :src="homePreview"
        :alt="t('onboarding.productOverview.previewAlt')"
        width="2240"
        height="1600"
        draggable="false"
        class="pointer-events-none block w-[56rem] max-w-none select-none"
      />
    </div>
    <figcaption v-if="locale !== 'en'" class="absolute right-4 top-4 rounded-full bg-background/95 px-3 py-1 text-xs text-foreground shadow-sm">
      {{ t('onboarding.productOverview.previewEnglishNote') }}
    </figcaption>
  </figure>
</template>

<style scoped>
.home-preview-crop {
  height: 34rem;
  /* Alpha reveals the selected shell atmosphere rather than painting over it. */
  mask-image: linear-gradient(to bottom, black calc(100% - 4rem), transparent 100%);
}
</style>
