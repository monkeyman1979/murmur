<script setup lang="ts">
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ArrowLeft } from "lucide-vue-next";
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <div class="flex min-w-0 flex-col gap-4 p-4">
    <div>
      <Button as-child size="sm" variant="ghost" class="-ml-2">
        <RouterLink to="/settings/about">
          <ArrowLeft class="size-4" aria-hidden="true" />
          {{ t('acknowledgements.back') }}
        </RouterLink>
      </Button>
    </div>

    <Card>
      <CardHeader class="pb-2">
        <CardTitle class="text-base">Murmur</CardTitle>
      </CardHeader>
      <CardContent class="flex flex-col gap-2 text-sm leading-6 text-muted-foreground">
        <p>{{ t('acknowledgements.original') }}</p>
        <p>{{ t('acknowledgements.license') }}</p>
      </CardContent>
    </Card>

    <Card>
      <CardHeader class="pb-2">
        <CardTitle class="text-base">{{ t('acknowledgements.thankYou') }}</CardTitle>
      </CardHeader>
      <CardContent class="flex flex-col gap-2 text-sm leading-6 text-muted-foreground">
        <p>{{ t('acknowledgements.contributors') }}</p>
        <p>{{ t('acknowledgements.notices') }}</p>
      </CardContent>
    </Card>
  </div>
</template>
