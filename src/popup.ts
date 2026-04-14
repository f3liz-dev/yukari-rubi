import Browser from "webextension-polyfill"

const dot = document.getElementById("dot")!
const statusText = document.getElementById("status-text")!
const toggleBtn = document.getElementById("toggle-btn")!
const mutationCb = document.getElementById("mutation-cb") as HTMLInputElement
const errorEl = document.getElementById("error")!
const sizeDecBtn = document.getElementById("size-dec")!
const sizeIncBtn = document.getElementById("size-inc")!
const sizeValueEl = document.getElementById("size-value")!
const sizeWarningEl = document.getElementById("size-warning")!

const addCurrentSiteBtn = document.getElementById("add-current-site")!
const patternInput = document.getElementById("pattern-input") as HTMLInputElement
const addPatternBtn = document.getElementById("add-pattern-btn")!
const patternListEl = document.getElementById("pattern-list")!

const SIZE_STEP = 10
const SIZE_MIN = 10
const SIZE_MAX = 200
const SIZE_WARNING_THRESHOLD = 100

let currentSize = 50

function t(key: string): string {
  return Browser.i18n.getMessage(key) || key
}

function applyI18n(): void {
  for (const el of document.querySelectorAll<HTMLElement>("[data-i18n]")) {
    const key = el.dataset.i18n!
    const msg = t(key)
    if (msg !== key) {
      el.textContent = msg
    }
  }
}

function updateSizeUI(size: number): void {
  currentSize = size
  sizeValueEl.textContent = `${size}%`
  sizeWarningEl.hidden = size < SIZE_WARNING_THRESHOLD
  sizeDecBtn.toggleAttribute("disabled", size <= SIZE_MIN)
  sizeIncBtn.toggleAttribute("disabled", size >= SIZE_MAX)
}

function updateUI(isActive: boolean): void {
  dot.classList.toggle("active", isActive)
  statusText.textContent = isActive ? t("statusActive") : t("statusInactive")
  toggleBtn.textContent = isActive ? t("disableFurigana") : t("enableFurigana")
}

function showError(msg: string): void {
  errorEl.textContent = msg
  errorEl.hidden = false
}

async function init(): Promise<void> {
  applyI18n()

  try {
    const tabs = await Browser.tabs.query({
      active: true,
      currentWindow: true,
    })
    const tabId = tabs[0]?.id
    if (tabId !== undefined) {
      try {
        const status: { active?: boolean } = await Browser.tabs.sendMessage(
          tabId,
          { type: "getStatus" },
        )
        updateUI(status?.active ?? false)
      } catch {
        updateUI(false)
      }
    }

    const settings: { mutationObserver?: boolean; rubySize?: number; autoEnablePatterns?: string[] } =
      await Browser.storage.local.get(["mutationObserver", "rubySize", "autoEnablePatterns"])
    mutationCb.checked = settings.mutationObserver ?? false
    updateSizeUI(settings.rubySize ?? 50)
    patterns = settings.autoEnablePatterns ?? []
    renderPatterns()
  } catch (err) {
    showError(String(err))
  }
}

toggleBtn.addEventListener("click", async () => {
  try {
    const tabs = await Browser.tabs.query({
      active: true,
      currentWindow: true,
    })
    const tabId = tabs[0]?.id
    if (tabId !== undefined) {
      await Browser.tabs.sendMessage(tabId, { type: "toggle" })
      await new Promise((r) => setTimeout(r, 150))
      try {
        const status: { active?: boolean } = await Browser.tabs.sendMessage(
          tabId,
          { type: "getStatus" },
        )
        updateUI(status?.active ?? false)
      } catch {
        const wasInactive = statusText.textContent === t("statusInactive")
        updateUI(wasInactive)
      }
    }
  } catch (err) {
    showError(String(err))
  }
})

mutationCb.addEventListener("change", () => {
  void Browser.storage.local.set({ mutationObserver: mutationCb.checked })
})

function changeSize(delta: number): void {
  const newSize = Math.max(SIZE_MIN, Math.min(SIZE_MAX, currentSize + delta))
  if (newSize !== currentSize) {
    updateSizeUI(newSize)
    void Browser.storage.local.set({ rubySize: newSize })
  }
}

sizeDecBtn.addEventListener("click", () => changeSize(-SIZE_STEP))
sizeIncBtn.addEventListener("click", () => changeSize(SIZE_STEP))

// --- Auto-enable patterns ---

let patterns: string[] = []

function renderPatterns(): void {
  patternListEl.innerHTML = ""
  for (const pattern of patterns) {
    const item = document.createElement("div")
    item.className = "pattern-item"
    const span = document.createElement("span")
    span.textContent = pattern
    span.title = pattern
    const btn = document.createElement("button")
    btn.textContent = "\u00d7"
    btn.addEventListener("click", () => removePattern(pattern))
    item.appendChild(span)
    item.appendChild(btn)
    patternListEl.appendChild(item)
  }
}

function savePatterns(): void {
  void Browser.storage.local.set({ autoEnablePatterns: patterns })
}

function addPattern(pattern: string): void {
  const trimmed = pattern.trim()
  if (!trimmed || patterns.includes(trimmed)) return
  patterns.push(trimmed)
  savePatterns()
  renderPatterns()
}

function removePattern(pattern: string): void {
  patterns = patterns.filter((p) => p !== pattern)
  savePatterns()
  renderPatterns()
}

addPatternBtn.addEventListener("click", () => {
  addPattern(patternInput.value)
  patternInput.value = ""
})

patternInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    addPattern(patternInput.value)
    patternInput.value = ""
  }
})

addCurrentSiteBtn.addEventListener("click", async () => {
  const tabs = await Browser.tabs.query({ active: true, currentWindow: true })
  const url = tabs[0]?.url
  if (!url) return
  try {
    const u = new URL(url)
    addPattern(`${u.protocol}//${u.hostname}/**`)
  } catch {
    // ignore invalid URLs
  }
})

void init()
