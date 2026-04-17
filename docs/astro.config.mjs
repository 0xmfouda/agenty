import { defineConfig } from "astro/config";
import starlight from "@astrojs/starlight";

export default defineConfig({
  integrations: [
    starlight({
      title: "Agenty",
      social: [
        {
          icon: "github",
          label: "GitHub",
          href: "https://github.com/anthropics/agenty",
        },
      ],
      sidebar: [
        {
          label: "Guides",
          items: [
            { label: "Getting Started", slug: "guides/getting-started" },
            { label: "Built-in Tools", slug: "guides/tools" },
            { label: "Plugins", slug: "guides/plugins" },
          ],
        },
        {
          label: "Reference",
          autogenerate: { directory: "reference" },
        },
      ],
    }),
  ],
});
