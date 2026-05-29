import Link from '@docusaurus/Link';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import useBaseUrl from '@docusaurus/useBaseUrl';
import Layout from '@theme/Layout';
import CodeBlock from '@theme/CodeBlock';

const features = [
  {
    icon: '📄',
    title: 'One file, three formats',
    description: 'Describe your whole stack in YAML, JSON or TOML. The same schema, your choice of syntax.',
  },
  {
    icon: '🧱',
    title: 'Apps, addons & network groups',
    description: 'Create and reconcile all three from one descriptor, with dependencies and service links wired automatically.',
  },
  {
    icon: '🔀',
    title: 'Environments from one file',
    description: (
      <>
        Template names and values with <code>{'${env}'}</code> and flip the whole stack with{' '}
        <code>--env prod</code>, <code>--env staging</code> or <code>--env dev</code>.
      </>
    ),
  },
  {
    icon: '🔑',
    title: 'Variables & secrets',
    description: (
      <>
        Reusable variables, per-env overrides, and a git-ignored <code>.secrets</code> sidecar referenced
        as <code>{'${secrets.key}'}</code>.
      </>
    ),
  },
  {
    icon: '🔗',
    title: 'Cross-resource references',
    description: (
      <>
        Pull a value live from another app or addon — <code>{'${addons.db.env.POSTGRESQL_ADDON_HOST}'}</code> —
        and have it injected at apply time.
      </>
    ),
  },
  {
    icon: '📝',
    title: 'Dry-run plans',
    description: 'A Terraform-style diff of exactly what will be created, updated or left alone before anything happens.',
  },
  {
    icon: '🔍',
    title: 'Drift detection',
    description: 'status compares your file against the live org and reports what changed, what is missing, and what is orphaned.',
  },
  {
    icon: '🔄',
    title: 'Reverse-engineering',
    description: 'read generates a project file from an existing org so you can adopt clever-project on a stack you already have.',
  },
  {
    icon: '⚙️',
    title: 'Hooks & CI-friendly',
    description: (
      <>
        Run your own commands around apply/delete, emit <code>--format json</code>, validate statically with{' '}
        <code>check</code>, and gate CI with <code>--exit-on-drift</code>.
      </>
    ),
  },
];

function HeroBanner() {
  const {siteConfig} = useDocusaurusContext();
  const logoUrl = useBaseUrl('img/logo.svg');
  return (
    <header className="hero-banner">
      <div className="container">
        <img src={logoUrl} alt="clever-project" className="hero-logo" />
        <h1>{siteConfig.title}</h1>
        <p>{siteConfig.tagline}</p>
        <div className="hero-buttons">
          <Link className="button button--primary button--lg" to="/docs/quick-start">
            Quick start
          </Link>
          <Link className="button button--secondary button--lg" to="/docs/introduction">
            Read the docs
          </Link>
        </div>
      </div>
    </header>
  );
}

function FeaturesSection() {
  return (
    <section className="features-section">
      <div className="container">
        <h2>A small declarative layer on top of Clever Cloud</h2>
        <div className="row">
          {features.map((f, i) => (
            <div className="col col--4" key={i}>
              <div className="feature-card">
                <span className="feature-icon">{f.icon}</span>
                <h3>{f.title}</h3>
                <p>{f.description}</p>
              </div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

function ExampleSection() {
  const example = `name: my-api
org: orga_xxxxxx-xxxx-xxxxx
region: par
variables:
  slug: \${ulid_lowercase()}
apps:
  api:
    name: api-\${slug}
    kind: node
    source:
      from: https://github.com/me/my-api.git
    domains:
      - api-\${slug}.cleverapps.io
    dependencies:
      - db
    env:
      DB_URI: \${addons.db.env.POSTGRESQL_ADDON_URI}
addons:
  db:
    name: db-\${slug}
    kind: postgresql
    size: xs_sml`;

  return (
    <section className="example-section">
      <div className="container">
        <h2>One file describes the whole stack</h2>
        <p className="example-lead">
          One app, one database, wired together, with a generated slug for a unique domain.
          Run <code>clever-project apply</code> and you get the app, the addon, the connection details
          injected into the app's env, the domain attached, and the deploy kicked off — in the right order.
        </p>
        <CodeBlock language="yaml">{example}</CodeBlock>
      </div>
    </section>
  );
}

function QuickStartSection() {
  return (
    <section className="quickstart-section">
      <div className="container">
        <h2>Up and running in four commands</h2>
        <CodeBlock language="bash">
          {`clever-project init                 # scaffold a project file
clever-project apply --env prod --dry-run   # preview the plan
clever-project apply --env prod     # create/update everything
clever-project status               # check the org still matches the file`}
        </CodeBlock>
        <p style={{textAlign: 'center', marginTop: '1.5rem'}}>
          <Link className="button button--primary" to="/docs/quick-start">
            See the full quick start →
          </Link>
        </p>
      </div>
    </section>
  );
}

function CtaSection() {
  return (
    <section className="cta-section">
      <div className="container">
        <h2>Open source</h2>
        <p>
          clever-project is a Rust CLI, MIT-style licensed and developed in the open.
          Install it from crates.io or grab a pre-built binary from the GitHub releases.
        </p>
        <div className="hero-buttons">
          <Link className="button button--primary button--lg" to="/docs/installation">
            Install clever-project
          </Link>
          <Link
            className="button button--outline button--primary button--lg"
            href="https://github.com/mathieuancelin/clever-project">
            View on GitHub
          </Link>
        </div>
      </div>
    </section>
  );
}

export default function Home() {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout description={siteConfig.tagline}>
      <HeroBanner />
      <main>
        <FeaturesSection />
        <ExampleSection />
        <QuickStartSection />
        <CtaSection />
      </main>
    </Layout>
  );
}
