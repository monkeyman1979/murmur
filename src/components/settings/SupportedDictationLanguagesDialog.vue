<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { Button } from "@/components/ui/button";
import {
  Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter,
  DialogHeader, DialogTitle, DialogTrigger,
} from "@/components/ui/dialog";
import { displayDictationLanguages } from "@/lib/dictationLanguages";
import { supportedAppLanguages } from "@/i18n/appLanguage";

const appLanguageCodes = new Set<string>(supportedAppLanguages.map(({ code }) => code));

const { t, locale } = useI18n();
const open = ref(false);
const search = ref("");
watch(open, (isOpen) => { if (!isOpen) search.value = ""; });
const languages = computed(() => displayDictationLanguages(locale.value));
const filtered = computed(() => {
  const query = search.value.trim().toLocaleLowerCase(locale.value);
  return query
    ? languages.value.filter(({ name }) => name.toLocaleLowerCase(locale.value).includes(query))
    : languages.value;
});
const appAndModelLanguages = computed(() => filtered.value.filter(({ code }) => appLanguageCodes.has(code)));
const modelOnlyLanguages = computed(() => filtered.value.filter(({ code }) => !appLanguageCodes.has(code)));
</script>

<template>
  <Dialog v-model:open="open">
    <DialogTrigger as-child>
      <Button variant="outline" size="sm">{{ t('settingsDictationLanguage.showLanguages') }}</Button>
    </DialogTrigger>
    <DialogContent :show-close-button="false" class="max-h-[min(85svh,44rem)] grid-rows-[auto_auto_minmax(0,1fr)_auto] sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>{{ t('settingsDictationLanguage.dialogTitle') }}</DialogTitle>
        <DialogDescription as="div" class="space-y-2 text-left">
          <p>{{ t('settingsDictationLanguage.dialogExplanation') }}</p>
          <p>{{ t('settingsDictationLanguage.dialogExample') }}</p>
          <p>{{ t('settingsDictationLanguage.dialogLimit') }}</p>
        </DialogDescription>
      </DialogHeader>
      <input
        v-model="search"
        type="search"
        class="w-full rounded-md border border-input bg-background px-3 py-2 text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        :aria-label="t('settingsDictationLanguage.search')"
        :placeholder="t('settingsDictationLanguage.search')"
      />
      <div class="min-h-0 overflow-y-auto" role="region" :aria-label="t('settingsDictationLanguage.listLabel')">
        <template v-if="filtered.length">
          <section v-if="appAndModelLanguages.length" aria-labelledby="app-and-model-languages">
            <h3 id="app-and-model-languages" class="mb-2 text-sm font-semibold">{{ t('settingsDictationLanguage.appAndModelLanguages') }}</h3>
            <ul class="grid grid-cols-2 gap-x-4 gap-y-2 py-1 text-sm sm:grid-cols-3">
              <li v-for="language in appAndModelLanguages" :key="language.code">{{ language.name }}</li>
            </ul>
          </section>
          <section v-if="modelOnlyLanguages.length" class="mt-4" aria-labelledby="model-only-languages">
            <h3 id="model-only-languages" class="mb-2 text-sm font-semibold">{{ t('settingsDictationLanguage.modelOnlyLanguages') }}</h3>
            <ul class="grid grid-cols-2 gap-x-4 gap-y-2 py-1 text-sm sm:grid-cols-3">
              <li v-for="language in modelOnlyLanguages" :key="language.code">{{ language.name }}</li>
            </ul>
          </section>
        </template>
        <p v-else class="py-4 text-sm text-muted-foreground" role="status">{{ t('settingsDictationLanguage.noMatches') }}</p>
      </div>
      <DialogFooter>
        <DialogClose as-child>
          <Button variant="outline">{{ t('settingsDictationLanguage.close') }}</Button>
        </DialogClose>
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
