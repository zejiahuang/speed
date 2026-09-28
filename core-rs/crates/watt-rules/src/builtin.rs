//! Curated rule set compiled into the binary.
//!
//! This is deliberately small and deliberately **address free**. Concrete CDN
//! addresses go stale within weeks, and a stale address is worse than no address
//! because it turns a working request into a timeout. The builtin set therefore
//! only marks domains as rule owned; the real, address bearing data arrives from
//! the remote update.
//!
//! The builtin set exists so that a first launch before any network round trip
//! still has a sensible notion of which domains the product cares about.

/// JSON payload of the builtin rule set.
pub const BUILTIN_RULES_JSON: &str = r#"
{
  "meta": { "version": "builtin-0.1.0", "update_time": "static" },
  "groups": [
    {
      "group": "developer",
      "groupZh": "开发者资源",
      "category": "MOBILE_USEFUL",
      "entries": [
        { "id": "b1001", "name": "GitHub.com", "nameZh": "GitHub.com", "domains": ["github.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "github.com,*.github.com", "isPlaceholder": true },
        { "id": "b1002", "name": "GitHub API", "nameZh": "GitHub API", "domains": ["api.github.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "api.github.com", "isPlaceholder": true },
        { "id": "b1003", "name": "GitHub raw", "nameZh": "GitHub raw", "domains": ["raw.githubusercontent.com", "gist.githubusercontent.com", "objects.githubusercontent.com", "media.githubusercontent.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "raw.githubusercontent.com,*.githubusercontent.com", "isPlaceholder": true },
        { "id": "b1004", "name": "GitHub assets", "nameZh": "GitHub 静态资源", "domains": ["github.githubassets.com", "assets-cdn.github.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "github.githubassets.com", "isPlaceholder": true },
        { "id": "b1005", "name": "GitHub release assets", "nameZh": "GitHub Release 下载", "domains": ["codeload.github.com", "release-assets.githubusercontent.com", "github-releases.githubusercontent.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "codeload.github.com", "isPlaceholder": true },
        { "id": "b1006", "name": "GitHub Copilot", "nameZh": "GitHub Copilot", "domains": ["api.individual.githubcopilot.com", "proxy.individual.githubcopilot.com", "origin-tracker.individual.githubcopilot.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "api.individual.githubcopilot.com", "isPlaceholder": true },
        { "id": "b1007", "name": "GitLab", "nameZh": "GitLab", "domains": ["gitlab.com", "registry.gitlab.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "gitlab.com", "isPlaceholder": true },
        { "id": "b1008", "name": "Hugging Face", "nameZh": "Hugging Face 模型下载", "domains": ["huggingface.co", "cdn-lfs.huggingface.co", "cdn-lfs-us-1.huggingface.co"], "ips": ["{Cloudfront}"], "port": "443", "cert": "huggingface.co", "isPlaceholder": true },
        { "id": "b1009", "name": "PyTorch", "nameZh": "PyTorch 下载", "domains": ["download.pytorch.org"], "ips": ["{Cloudfront}"], "port": "443", "cert": "download.pytorch.org", "isPlaceholder": true },
        { "id": "b1010", "name": "Ollama", "nameZh": "Ollama", "domains": ["ollama.ai", "registry.ollama.ai"], "ips": ["{Cloudflare}"], "port": "443", "cert": "ollama.ai", "isPlaceholder": true },
        { "id": "b1011", "name": "JetBrains", "nameZh": "JetBrains 下载", "domains": ["download.jetbrains.com", "plugins.jetbrains.com", "cache-redirector.jetbrains.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "download.jetbrains.com", "isPlaceholder": true },
        { "id": "b1012", "name": "Stack Overflow", "nameZh": "Stack Overflow", "domains": ["stackoverflow.com", "stackexchange.com", "serverfault.com", "superuser.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "stackoverflow.com", "isPlaceholder": true },
        { "id": "b1013", "name": "npm registry", "nameZh": "npm 源", "domains": ["registry.npmjs.org", "registry.yarnpkg.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "registry.npmjs.org", "isPlaceholder": true },
        { "id": "b1014", "name": "crates.io", "nameZh": "crates.io", "domains": ["crates.io", "static.crates.io", "index.crates.io"], "ips": ["{Cloudfront}"], "port": "443", "cert": "crates.io", "isPlaceholder": true },
        { "id": "b1015", "name": "PyPI", "nameZh": "PyPI", "domains": ["pypi.org", "files.pythonhosted.org"], "ips": ["{Cloudfront}"], "port": "443", "cert": "pypi.org", "isPlaceholder": true },
        { "id": "b1016", "name": "Maven Central", "nameZh": "Maven 中央仓库", "domains": ["repo.maven.apache.org", "repo1.maven.org"], "ips": ["{Cloudflare}"], "port": "443", "cert": "repo.maven.apache.org", "isPlaceholder": true },
        { "id": "b1017", "name": "Docker Hub", "nameZh": "Docker Hub", "domains": ["registry-1.docker.io", "production.cloudflare.docker.com", "auth.docker.io"], "ips": ["{Cloudflare}"], "port": "443", "cert": "registry-1.docker.io", "isPlaceholder": true },
        { "id": "b1018", "name": "Flathub", "nameZh": "Flathub 下载", "domains": ["flathub.org", "dl.flathub.org"], "ips": ["{Cloudfront}"], "port": "443", "cert": "flathub.org", "isPlaceholder": true },
        { "id": "b1019", "name": "dl.google.com", "nameZh": "dl.google.com", "domains": ["dl.google.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "dl.google.com", "isPlaceholder": true },
        { "id": "b1020", "name": "LM Studio", "nameZh": "LM Studio", "domains": ["lmstudio.ai"], "ips": ["{Cloudflare}"], "port": "443", "cert": "lmstudio.ai", "isPlaceholder": true }
      ]
    },
    {
      "group": "CDN for open-source",
      "groupZh": "开源 CDN",
      "category": "MOBILE_USEFUL",
      "entries": [
        { "id": "b2001", "name": "Cdnjs", "nameZh": "Cdnjs", "domains": ["cdnjs.com", "cdnjs.cloudflare.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "cdnjs.cloudflare.com,*.cdnjs.cloudflare.com", "isPlaceholder": true },
        { "id": "b2002", "name": "UNPKG", "nameZh": "UNPKG", "domains": ["unpkg.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "unpkg.com,*.unpkg.com", "isPlaceholder": true },
        { "id": "b2003", "name": "jsDelivr", "nameZh": "jsDelivr", "domains": ["cdn.jsdelivr.net", "jsdelivr.net"], "ips": ["{Cloudflare}"], "port": "443", "cert": "cdn.jsdelivr.net,*.jsdelivr.net", "isPlaceholder": true },
        { "id": "b2004", "name": "jQuery", "nameZh": "jQuery", "domains": ["code.jquery.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "code.jquery.com,*.jquery.com", "isPlaceholder": true },
        { "id": "b2005", "name": "Font Awesome", "nameZh": "Font Awesome", "domains": ["use.fontawesome.com", "kit.fontawesome.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "use.fontawesome.com", "isPlaceholder": true },
        { "id": "b2006", "name": "Bootstrap", "nameZh": "Bootstrap", "domains": ["getbootstrap.com", "bootstrapcdn.com", "maxcdn.bootstrapcdn.com"], "ips": ["{Cloudfront}"], "port": "443", "cert": "getbootstrap.com", "isPlaceholder": true }
      ]
    },
    {
      "group": "Academic",
      "groupZh": "学术资源",
      "category": "MOBILE_USEFUL",
      "entries": [
        { "id": "b3001", "name": "ScienceDirect", "nameZh": "ScienceDirect", "domains": ["sciencedirect.com", "pdf.sciencedirectassets.com"], "ips": ["{Cloudflare}"], "port": "443", "cert": "sciencedirect.com", "isPlaceholder": true },
        { "id": "b3002", "name": "ResearchGate", "nameZh": "ResearchGate", "domains": ["researchgate.net", "rgstatic.net"], "ips": ["{Cloudflare}"], "port": "443", "cert": "researchgate.net", "isPlaceholder": true }
      ]
    }
  ]
}
"#;
