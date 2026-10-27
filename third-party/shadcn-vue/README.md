# Retained shadcn-vue assets

Murmur's locally adapted `src/components/ui/` components originate from shadcn-vue. They continue to use Reka UI; the generator CLI is not needed to run or build them.

`src/styles/shadcn-vue.css` is an unchanged copy of `dist/tailwind.css` from the MIT-licensed **shadcn-vue 2.8.2** npm package:
https://registry.npmjs.org/shadcn-vue/-/shadcn-vue-2.8.2.tgz

Upstream project: https://github.com/unovue/shadcn-vue

Stylesheet SHA-256: `e93f9da1091a7a124b9395adfdba6329c2a7b18aceee1acea95ccfa4b36a23ba`.

The accompanying `LICENSE` is copied unchanged from that package. The legal inventory generator includes it in the bundled third-party notices independently of whether the CLI is installed. Retain this attribution for both the copied components and stylesheet. Any future generator use should be a deliberate development-tool decision, not an implicit application-build dependency.
