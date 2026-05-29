/** @type {import('@docusaurus/plugin-content-docs').SidebarsConfig} */
const sidebars = {
  docSidebar: [
    'introduction',
    'installation',
    'quick-start',
    'project-file',
    {
      type: 'category',
      label: 'Commands',
      collapsed: false,
      items: [
        'commands/index',
        'commands/init',
        'commands/apply',
        'commands/delete',
        'commands/check',
        'commands/status',
        'commands/read',
      ],
    },
    'variables',
    'secrets',
    'cross-references',
    'managed-addons',
    'display',
    'hooks',
    'state',
    'json-output',
    'completions',
    'limitations',
    'contributing',
  ],
};

export default sidebars;
