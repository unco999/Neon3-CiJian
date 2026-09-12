(function () {
  "use strict";

  var root = document.documentElement;
  var body = document.body;
  var sidebar = document.getElementById("sidebar");
  var menuToggle = document.getElementById("menuToggle");
  var themeToggle = document.getElementById("themeToggle");
  var search = document.getElementById("docSearch");
  var searchStatus = document.getElementById("searchStatus");

  function setTheme(theme) {
    root.setAttribute("data-theme", theme);
    if (themeToggle) {
      themeToggle.textContent = theme === "dark" ? "浅色" : "深色";
      themeToggle.setAttribute("aria-label", theme === "dark" ? "切换到浅色主题" : "切换到深色主题");
    }
    try {
      window.localStorage.setItem("nui-flow-theme", theme);
    } catch (error) {
      return;
    }
  }

  var savedTheme = null;
  try {
    savedTheme = window.localStorage.getItem("nui-flow-theme");
  } catch (error) {
    savedTheme = null;
  }
  setTheme(savedTheme === "light" ? "light" : "dark");

  if (themeToggle) {
    themeToggle.addEventListener("click", function () {
      setTheme(root.getAttribute("data-theme") === "dark" ? "light" : "dark");
    });
  }

  function setMenu(open) {
    if (!sidebar || !menuToggle) {
      return;
    }
    sidebar.classList.toggle("is-open", open);
    body.classList.toggle("menu-open", open);
    menuToggle.setAttribute("aria-expanded", open ? "true" : "false");
  }

  if (menuToggle) {
    menuToggle.addEventListener("click", function () {
      setMenu(!sidebar.classList.contains("is-open"));
    });
  }

  document.querySelectorAll(".toc a, .topbar-nav a, .brand").forEach(function (link) {
    link.addEventListener("click", function () {
      setMenu(false);
    });
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") {
      setMenu(false);
    }
  });

  document.querySelectorAll(".code-wrap pre").forEach(function (pre) {
    var wrap = pre.parentElement;
    var button = document.createElement("button");
    button.type = "button";
    button.className = "copy-button";
    button.textContent = "复制";
    button.setAttribute("aria-label", "复制代码");
    button.addEventListener("click", function () {
      var text = pre.textContent;
      var done = function () {
        button.textContent = "已复制";
        button.classList.add("is-copied");
        window.setTimeout(function () {
          button.textContent = "复制";
          button.classList.remove("is-copied");
        }, 1400);
      };
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(text).then(done).catch(function () {
          fallbackCopy(text, done);
        });
      } else {
        fallbackCopy(text, done);
      }
    });
    wrap.appendChild(button);
  });

  function fallbackCopy(text, done) {
    var area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    try {
      document.execCommand("copy");
      done();
    } catch (error) {
      return;
    } finally {
      area.remove();
    }
  }

  function filterSections(query) {
    var normalized = query.trim().toLowerCase();
    var sections = Array.prototype.slice.call(document.querySelectorAll("[data-search-section]"));
    if (!normalized) {
      sections.forEach(function (section) {
        section.classList.remove("is-hidden-by-search");
      });
      if (searchStatus) {
        searchStatus.textContent = "搜索 15 个章节";
      }
      return;
    }
    var count = 0;
    sections.forEach(function (section) {
      var match = section.textContent.toLowerCase().indexOf(normalized) !== -1;
      var keepSearchBox = section.id === "start";
      section.classList.toggle("is-hidden-by-search", !match && !keepSearchBox);
      if (match) {
        count += 1;
      }
    });
    if (searchStatus) {
      searchStatus.textContent = count + " 个章节命中";
    }
  }

  if (search) {
    search.addEventListener("input", function () {
      filterSections(search.value);
    });
  }

  document.querySelectorAll("[data-filter]").forEach(function (button) {
    button.addEventListener("click", function () {
      var filter = button.getAttribute("data-filter");
      document.querySelectorAll("[data-filter]").forEach(function (item) {
        item.classList.toggle("is-active", item === button);
      });
      document.querySelectorAll("#capabilityMatrix [data-status]").forEach(function (card) {
        card.classList.toggle("is-hidden-by-search", filter !== "all" && card.getAttribute("data-status") !== filter);
      });
    });
  });

  document.querySelectorAll("[data-tab-target]").forEach(function (button) {
    button.addEventListener("click", function () {
      var target = button.getAttribute("data-tab-target");
      var group = button.getAttribute("data-tab-group");
      document.querySelectorAll("[data-tab-group='" + group + "']").forEach(function (item) {
        item.classList.toggle("is-active", item === button || item.getAttribute("data-tab-panel") === target);
      });
    });
  });

  var tocLinks = Array.prototype.slice.call(document.querySelectorAll("[data-toc-link]"));
  var observedSections = Array.prototype.slice.call(document.querySelectorAll("main > section[id]"));
  if (window.IntersectionObserver) {
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) {
          return;
        }
        tocLinks.forEach(function (link) {
          link.classList.toggle("is-active", link.getAttribute("href") === "#" + entry.target.id);
        });
      });
    }, { rootMargin: "-90px 0px -65% 0px", threshold: 0 });
    observedSections.forEach(function (section) {
      observer.observe(section);
    });
  }

  document.querySelectorAll("img").forEach(function (image) {
    image.addEventListener("error", function () {
      image.style.display = "none";
      var caption = image.parentElement.querySelector("figcaption");
      if (caption) {
        caption.insertAdjacentHTML("afterbegin", "<strong>渲染图暂不可用</strong>");
      }
    });
  });
}());
