(() => {
  // Load mermaid from CDN and initialize
  const loadMermaidAndInit = () => {
    // Check if mermaid is already loaded
    if (typeof mermaid !== "undefined") {
      initializeMermaid();
      return;
    }

    // Load mermaid from CDN
    const script = document.createElement("script");
    script.src =
      "https://cdn.jsdelivr.net/npm/mermaid@11.9.0/dist/mermaid.min.js";
    script.onload = () => {
      initializeMermaid();
    };
    script.onerror = () => {
      console.error("Failed to load mermaid from CDN");
    };
    document.head.appendChild(script);
  };

  const initializeMermaid = () => {
    const darkThemes = ["ayu", "navy", "coal"];
    const lightThemes = ["light", "rust"];

    const classList = document.getElementsByTagName("html")[0].classList;

    let lastThemeWasLight = true;
    for (const cssClass of classList) {
      if (darkThemes.includes(cssClass)) {
        lastThemeWasLight = false;
        break;
      }
    }

    const theme = lastThemeWasLight ? "default" : "dark";

    // Convert code blocks with language-mermaid to mermaid divs
    document.querySelectorAll("pre code.language-mermaid").forEach((block) => {
      const pre = block.parentElement;
      const mermaidDiv = document.createElement("div");
      mermaidDiv.className = "mermaid";
      mermaidDiv.textContent = block.textContent;
      pre.parentNode.replaceChild(mermaidDiv, pre);
    });

    try {
      mermaid.initialize({
        startOnLoad: true,
        theme,
        securityLevel: "loose",
      });
    } catch (error) {
      console.error("Failed to initialize mermaid:", error);
    }

    // Simplest way to make mermaid re-render the diagrams in the new theme is via refreshing the page
    for (const darkTheme of darkThemes) {
      const element = document.getElementById(darkTheme);
      if (element) {
        element.addEventListener("click", () => {
          if (lastThemeWasLight) {
            window.location.reload();
          }
        });
      }
    }

    for (const lightTheme of lightThemes) {
      const element = document.getElementById(lightTheme);
      if (element) {
        element.addEventListener("click", () => {
          if (!lastThemeWasLight) {
            window.location.reload();
          }
        });
      }
    }
  };

  // Initialize when page loads
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", loadMermaidAndInit);
  } else {
    loadMermaidAndInit();
  }
})();
