import {themes as prismThemes} from 'prism-react-renderer';

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'clever-project',
  tagline: 'Declare your Clever Cloud apps and addons in one file, then apply, diff and tear them down with a single command.',
  favicon: 'img/logo.svg',

  future: {
    v4: true,
  },

  url: 'https://mathieuancelin.github.io',
  baseUrl: '/clever-project/',

  organizationName: 'mathieuancelin',
  projectName: 'clever-project',

  onBrokenLinks: 'throw',

  markdown: {
    format: 'detect',
    hooks: {
      onBrokenMarkdownLinks: 'warn',
      onBrokenMarkdownImages: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  themes: [
    [
      require.resolve('@easyops-cn/docusaurus-search-local'),
      ({
        hashed: true,
        language: ['en'],
        indexBlog: false,
      }),
    ],
  ],

  presets: [
    [
      'classic',
      /** @type {import('@docusaurus/preset-classic').Options} */
      ({
        docs: {
          sidebarPath: './sidebars.js',
          editUrl: 'https://github.com/mathieuancelin/clever-project/tree/main/documentation/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      }),
    ],
  ],

  themeConfig:
    /** @type {import('@docusaurus/preset-classic').ThemeConfig} */
    ({
      image: 'img/logo.svg',
      colorMode: {
        defaultMode: 'light',
        respectPrefersColorScheme: true,
      },
      navbar: {
        title: 'clever-project',
        logo: {
          alt: 'clever-project',
          src: 'img/logo.svg',
        },
        items: [
          {
            type: 'docSidebar',
            sidebarId: 'docSidebar',
            position: 'left',
            label: 'Documentation',
          },
          {
            href: 'https://crates.io/crates/clever-project',
            label: 'crates.io',
            position: 'right',
          },
          {
            href: 'https://github.com/mathieuancelin/clever-project',
            label: 'GitHub',
            position: 'right',
          },
        ],
      },
      footer: {
        style: 'dark',
        links: [
          {
            title: 'Documentation',
            items: [
              {label: 'Introduction', to: '/docs/introduction'},
              {label: 'Quick start', to: '/docs/quick-start'},
              {label: 'Commands', to: '/docs/commands/'},
            ],
          },
          {
            title: 'Reference',
            items: [
              {label: 'Project file', to: '/docs/project-file'},
              {label: 'Variables', to: '/docs/variables'},
              {label: 'Secrets', to: '/docs/secrets'},
            ],
          },
          {
            title: 'More',
            items: [
              {label: 'GitHub', href: 'https://github.com/mathieuancelin/clever-project'},
              {label: 'crates.io', href: 'https://crates.io/crates/clever-project'},
              {label: 'Clever Cloud', href: 'https://www.clever.cloud/'},
            ],
          },
        ],
        copyright: `Copyright © ${new Date().getFullYear()} clever-project contributors.`,
      },
      prism: {
        theme: prismThemes.github,
        darkTheme: prismThemes.dracula,
        additionalLanguages: ['bash', 'json', 'yaml', 'toml'],
      },
    }),
};

export default config;
