import { CpuColorizer } from './cpu_colorizer.js';
import { KANAGAWA, WebGpuColorizer, hexToRgb, labToRgb, normalizeHex, parseColorscheme, rgbToLab } from './colorizer.js';

const elements = {
  form: document.querySelector('#form'),
  file: document.querySelector('#file'),
  compare: document.querySelector('#compare'),
  afterLayer: document.querySelector('#afterLayer'),
  divider: document.querySelector('#divider'),
  loupe: document.querySelector('#loupe'),
  toggleLoupe: document.querySelector('#toggleLoupe'),
  status: document.querySelector('#status'),
  inputCanvas: document.querySelector('#inputCanvas'),
  outputCanvas: document.querySelector('#outputCanvas'),
  download: document.querySelector('#download'),
  saveConfig: document.querySelector('#saveConfig'),
  importConfig: document.querySelector('#importConfig'),
  configFile: document.querySelector('#configFile'),
  schemeModal: document.querySelector('#schemeModal'),
  openScheme: document.querySelector('#openScheme'),
  closeScheme: document.querySelector('#closeScheme'),
  schemeBrowser: document.querySelector('#schemeBrowser'),
  schemeSearch: document.querySelector('#schemeSearch'),
  schemeDisplay: document.querySelector('#schemeDisplay'),
  activeSchemeStrip: document.querySelector('#activeSchemeStrip'),
  schemeName: document.querySelector('#schemeName'),
  schemeText: document.querySelector('#schemeText'),
  swatches: document.querySelector('#swatches'),
  newColor: document.querySelector('#newColor'),
  addColor: document.querySelector('#addColor'),
  sortColors: document.querySelector('#sortColors'),
  saveColorscheme: document.querySelector('#saveColorscheme'),
  interpolate: document.querySelector('#interpolate'),
  helpModal: document.querySelector('#helpModal'),
  helpTitle: document.querySelector('#helpTitle'),
  helpCopy: document.querySelector('#helpCopy'),
  helpExamples: document.querySelector('#helpExamples'),
  closeHelp: document.querySelector('#closeHelp'),
  fallbackModal: document.querySelector('#fallbackModal'),
  closeFallback: document.querySelector('#closeFallback'),
  cpuProgress: document.querySelector('#cpuProgress'),
  cpuProgressBar: document.querySelector('#cpuProgressBar'),
  openCpuExplain: document.querySelector('#openCpuExplain'),
  cpuExplainModal: document.querySelector('#cpuExplainModal'),
  closeCpuExplain: document.querySelector('#closeCpuExplain'),
};
const sliders = {
  blend: document.querySelector('#blend'),
  dither: document.querySelector('#dither'),
  radius: document.querySelector('#radius'),
  threshold: document.querySelector('#threshold'),
};
const values = {
  blend: document.querySelector('#blendValue'),
  dither: document.querySelector('#ditherValue'),
  radius: document.querySelector('#radiusValue'),
  threshold: document.querySelector('#thresholdValue'),
};
const help = {
  blend: {
    title: 'Blend',
    copy: 'Blend is a straight mix between the original image and the generated colorized image. At 0 you get the source unchanged. At 1 you get the full palette result. The values in between are crossfades.',
    labels: ['0.00 — original', '0.50', '1.00 — generated'],
    values: [0, 0.5, 1],
  },
  dither: {
    title: 'Dither',
    copy: 'Dither adds tiny, structured variation before palette matching. It can break up flat bands in gradients, but too much looks noisy.',
    labels: ['0.00 — clean bands', '0.18 — softened bands', '0.65 — visible grain'],
    values: [0, 0.18, 0.65],
  },
  radius: {
    title: 'Spatial radius',
    copy: 'Spatial radius averages nearby color decisions before restoring luminance. Small radii keep hard color boundaries. Large radii make smoother, more painterly chroma.',
    labels: ['0 — sharp', '10 — smooth', '45 — washed together'],
    values: [0, 10, 45],
  },
  threshold: {
    title: 'Palette detail',
    copy: 'Palette detail controls how many in-between colors are generated when Smooth palette ramps is on. Lower values add more intermediate colors for smoother ramps. Higher values stay closer to the original palette stops.',
    labels: ['0.8 — many steps', '6 — moderate', '35 — sparse'],
    values: [0.8, 6, 35],
  },
};

let colorizer;
let colorizerReady;
let inputImageData;
let inputFileName = 'image.png';
let inputLoupeUrl;
let outputLoupeUrl;
let outputDownloadUrl;
let renderTimer;
let renderInFlight = false;
let renderAgain = false;
let currentSplit = 50;
let dragging = false;
let loupeEnabled = false;
let cpuFallbackShown = false;
let schemes = [];

bootstrap();

async function bootstrap() {
  elements.schemeText.value = KANAGAWA.join('\n');
  restoreConfig();
  syncControls();
  bindEvents();
  setSplit(50);
  loadSchemeBrowser();

  colorizerReady = createColorizer()
    .then(instance => {
      colorizer = instance;
      setStatus(inputImageData ? 'Ready. Rendering…' : 'Ready.');
      if (inputImageData) scheduleRender(0);
    })
    .catch(error => setError(error.message || String(error)));
}
async function createColorizer() {
  try {
    return await WebGpuColorizer.create();
  } catch {
    const colorizer = await CpuColorizer.create();
    colorizer.onProgress = updateCpuProgress;


    showCpuFallbackModal();

    return colorizer;
  }
}

function showCpuFallbackModal() {
  if (cpuFallbackShown) return;

  cpuFallbackShown = true;
  elements.fallbackModal.showModal();
}


function bindEvents() {
  elements.form.addEventListener('submit', event => event.preventDefault());
  elements.file.addEventListener('change', loadImage);

  for (const slider of Object.values(sliders)) slider.addEventListener('input', scheduleRender);

  elements.interpolate.addEventListener('change', scheduleRender);
  elements.schemeText.addEventListener('input', scheduleRender);
  elements.schemeName.addEventListener('input', syncControls);
  elements.openScheme.addEventListener('click', () => elements.schemeModal.showModal());
  elements.closeScheme.addEventListener('click', () => elements.schemeModal.close());
  elements.schemeModal.addEventListener('click', event => { if (event.target === elements.schemeModal) elements.schemeModal.close(); });
  elements.schemeSearch.addEventListener('input', renderSchemeBrowser);
  elements.addColor.addEventListener('click', addColor);
  elements.sortColors.addEventListener('click', sortColors);
  elements.saveConfig.addEventListener('click', saveConfig);
  elements.importConfig.addEventListener('click', () => elements.configFile.click());
  elements.configFile.addEventListener('change', importConfig);
  elements.saveColorscheme.addEventListener('click', saveColorscheme);
  elements.toggleLoupe.addEventListener('click', toggleLoupe);
  elements.compare.addEventListener('pointerdown', pointerDown);
  elements.compare.addEventListener('pointermove', pointerMove);
  elements.compare.addEventListener('pointerup', pointerUp);
  elements.compare.addEventListener('pointercancel', pointerCancel);
  elements.compare.addEventListener('pointerleave', () => elements.loupe.classList.remove('on'));
  elements.compare.addEventListener('pointerenter', () => { if (loupeEnabled) elements.loupe.classList.add('on'); });
  elements.closeHelp.addEventListener('click', () => elements.helpModal.close());
  elements.helpModal.addEventListener('click', event => { if (event.target === elements.helpModal) elements.helpModal.close(); });
  elements.closeFallback.addEventListener('click', () => elements.fallbackModal.close());
  elements.openCpuExplain.addEventListener('click', () => {
    elements.fallbackModal.close();
    elements.cpuExplainModal.showModal();
  });
  elements.closeCpuExplain.addEventListener('click', () => elements.cpuExplainModal.close());
  elements.fallbackModal.addEventListener('click', event => { if (event.target === elements.fallbackModal) elements.fallbackModal.close(); });
  elements.cpuExplainModal.addEventListener('click', event => { if (event.target === elements.cpuExplainModal) elements.cpuExplainModal.close(); });

  for (const button of document.querySelectorAll('[data-help]')) button.addEventListener('click', () => openHelp(button.dataset.help));
}

async function loadImage() {
  const file = elements.file.files[0];
  if (!file) return;

  inputFileName = file.name;
  setStatus('Loading image…');

  try {
    inputImageData = await fileToImageData(file);
    setStatus(colorizer ? 'Rendering…' : 'Preparing…');
    drawImageData(elements.inputCanvas, inputImageData);
    replaceLoupeUrl('input', elements.inputCanvas.toDataURL('image/png'));
    elements.compare.classList.remove('empty');
    scheduleRender(0);
  } catch (error) {
    setError(error.message || String(error));
  }
}

function scheduleRender(delay = 120) {
  syncControls();
  clearTimeout(renderTimer);
  renderTimer = setTimeout(renderImage, delay);
}

async function renderImage() {
  if (!inputImageData) return;

  if (renderInFlight) {
    renderAgain = true;
    return;
  }

  renderInFlight = true;
  renderAgain = false;

  try {
    if (!colorizer) {
      setStatus('Preparing…');
      await colorizerReady;
      if (!colorizer) return;
    }

    if (colorizer.mode === 'cpu') showCpuProgress(0);

    setStatus('Rendering…');
    const output = await colorizer.colorize(inputImageData, currentOptions());

    drawImageData(elements.outputCanvas, output);
    replaceLoupeUrl('output', elements.outputCanvas.toDataURL('image/png'));
    await updateDownload();
    hideCpuProgress();
    setStatus('Rendered.');
  } catch (error) {
    hideCpuProgress();
    setError(error.message || String(error));
  } finally {
    renderInFlight = false;

    if (renderAgain) {
      renderAgain = false;
      scheduleRender(0);
    }
  }
}

function showCpuProgress(progress) {
  elements.cpuProgress.hidden = false;
  updateCpuProgress(progress);
}

function updateCpuProgress(progress) {
  elements.cpuProgressBar.style.width = `${Math.round(Math.max(0, Math.min(1, progress)) * 100)}%`;
}

function hideCpuProgress() {
  elements.cpuProgress.hidden = true;
  updateCpuProgress(0);
}

function currentOptions() {
  return {
    blendFactor: Number(sliders.blend.value),
    ditherAmount: Number(sliders.dither.value),
    spatialRadius: Number(sliders.radius.value),
    interpolationThreshold: Number(sliders.threshold.value),
    interpolateColors: elements.interpolate.checked,
    colors: parseColorscheme(elements.schemeText.value),
  };
}

function syncControls() {
  values.blend.textContent = sliders.blend.value;
  values.dither.textContent = sliders.dither.value;
  values.radius.textContent = sliders.radius.value;
  values.threshold.textContent = sliders.threshold.value;
  elements.schemeDisplay.textContent = elements.schemeName.value || 'Custom';

  try {
    const palette = parseColorscheme(elements.schemeText.value);
    renderSwatches(palette);
    applyTheme(palette);
  } catch {
    elements.activeSchemeStrip.innerHTML = '';
    elements.swatches.innerHTML = '';
  }
}

function renderSwatches(palette) {
  elements.activeSchemeStrip.innerHTML = schemeStripSpans(palette);
  elements.swatches.innerHTML = '';

  for (const color of palette) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'swatch';
    button.style.background = color;
    button.title = `${color} — click to remove`;
    button.addEventListener('click', () => {
      elements.schemeText.value = elements.schemeText.value.split('\n').filter(line => line.trim() !== color && line.trim() !== color.slice(1)).join('\n');
      scheduleRender();
    });
    elements.swatches.append(button);
  }
}

async function loadSchemeBrowser() {
  const local = localSchemes();

  try {
    const response = await fetch('https://api.github.com/repos/tinted-theming/schemes/contents/base16');
    if (!response.ok) throw new Error(`GitHub returned ${response.status}`);

    const contents = await response.json();
    const remote = contents
      .filter(item => item.name.endsWith('.yaml') || item.name.endsWith('.yml'))
      .map(item => ({
        name: item.name.replace(/\.ya?ml$/, ''),
        url: item.download_url,
        source: 'Tinted Base16',
      }));

    schemes = [...local, ...await loadRemotePreviews(remote)];
    renderSchemeBrowser();
  } catch (error) {
    schemes = local;
    elements.schemeBrowser.innerHTML = `<p class="hint">Could not load remote colorschemes: ${error.message || error}</p>`;
    if (local.length) renderSchemeBrowser();
  }
}

async function loadRemotePreviews(items) {
  let next = 0;
  const output = [];
  const workers = Array.from({ length: 12 }, async () => {
    while (next < items.length) {
      const item = items[next++];

      try {
        const text = await fetchText(item.url);
        output.push({ ...item, text, colors: parseBase16Yaml(text) });
      } catch {
      }
    }
  });

  await Promise.all(workers);
  return output.sort((left, right) => left.name.localeCompare(right.name));
}

function renderSchemeBrowser() {
  const query = elements.schemeSearch.value.trim().toLowerCase();
  const filtered = query ? schemes.filter(scheme => scheme.name.toLowerCase().includes(query)) : schemes;

  elements.schemeBrowser.innerHTML = '';

  if (!filtered.length) {
    elements.schemeBrowser.innerHTML = '<p class="hint">No colorschemes match that search.</p>';
    return;
  }

  for (const scheme of filtered) {
    const card = document.createElement('button');
    card.type = 'button';
    card.className = 'scheme-card';
    card.innerHTML = `<strong>${escapeHtml(scheme.name)}</strong><div class="scheme-strip">${schemeStripSpans(scheme.colors)}</div><span class="hint">${scheme.source}</span>`;
    card.addEventListener('click', () => selectScheme(scheme));
    elements.schemeBrowser.append(card);
  }
}

function selectScheme(scheme) {
  elements.schemeName.value = scheme.name;
  elements.schemeText.value = scheme.text || scheme.colors.join('\n');
  scheduleRender();
}

function addColor() {
  try {
    const color = normalizeHex(elements.newColor.value.trim());
    elements.schemeText.value = `${elements.schemeText.value.trim()}\n${color}`.trim();
    elements.newColor.value = '';
    scheduleRender();
  } catch (error) {
    setError(error.message || String(error));
  }
}

function sortColors() {
  try {
    elements.schemeText.value = parseColorscheme(elements.schemeText.value).sort((left, right) => luminance(left) - luminance(right)).join('\n');
    scheduleRender();
  } catch (error) {
    setError(error.message || String(error));
  }
}

function saveConfig() {
  const config = {
    blendFactor: Number(sliders.blend.value),
    ditherAmount: Number(sliders.dither.value),
    spatialRadius: Number(sliders.radius.value),
    interpolateColors: elements.interpolate.checked,
    interpolationThreshold: Number(sliders.threshold.value),
    colorschemeName: elements.schemeName.value,
    colorschemeText: elements.schemeText.value,
  };

  localStorage.setItem('image-colorizer:config', JSON.stringify(config));
  downloadText('image-colorizer-config.json', JSON.stringify(config, null, 2), 'application/json');
  setStatus('Saved config.');
}

function restoreConfig() {
  const saved = localStorage.getItem('image-colorizer:config');
  if (!saved) return;

  try {
    applyConfig(JSON.parse(saved));
  } catch {
  }
}

async function importConfig() {
  const file = elements.configFile.files[0];
  if (!file) return;

  try {
    applyConfig(JSON.parse(await file.text()));
    scheduleRender();
    setStatus('Imported config.');
  } catch (error) {
    setError(error.message || String(error));
  } finally {
    elements.configFile.value = '';
  }
}

function applyConfig(config) {
  sliders.blend.value = config.blendFactor ?? sliders.blend.value;
  sliders.dither.value = config.ditherAmount ?? sliders.dither.value;
  sliders.radius.value = config.spatialRadius ?? sliders.radius.value;
  sliders.threshold.value = config.interpolationThreshold ?? sliders.threshold.value;
  elements.interpolate.checked = config.interpolateColors ?? elements.interpolate.checked;
  elements.schemeName.value = config.colorschemeName ?? elements.schemeName.value;
  elements.schemeText.value = config.colorschemeText ?? elements.schemeText.value;
  syncControls();
}

function saveColorscheme() {
  const name = sanitizeName(elements.schemeName.value || 'custom');
  const text = elements.schemeText.value.trim();

  try {
    parseColorscheme(text);
    localStorage.setItem(`image-colorizer:scheme:${name}`, text);
    downloadText(`${name}.txt`, `${text}\n`, 'text/plain');
    schemes = [...localSchemes(), ...schemes.filter(scheme => scheme.source !== 'Saved locally')];
    renderSchemeBrowser();
    setStatus('Saved colorscheme.');
  } catch (error) {
    setError(error.message || String(error));
  }
}

function localSchemes() {
  const output = [];

  for (let index = 0; index < localStorage.length; index++) {
    const key = localStorage.key(index);
    if (!key?.startsWith('image-colorizer:scheme:')) continue;

    const text = localStorage.getItem(key) || '';
    try {
      output.push({
        name: key.slice('image-colorizer:scheme:'.length),
        text,
        colors: parseColorscheme(text),
        source: 'Saved locally',
      });
    } catch {
    }
  }

  return output.sort((left, right) => left.name.localeCompare(right.name));
}

function setSplit(percent) {
  const clamped = Math.max(0, Math.min(100, percent));
  currentSplit = clamped;
  elements.afterLayer.style.clipPath = `inset(0 0 0 ${clamped}%)`;
  elements.divider.style.left = `${clamped}%`;
  elements.divider.setAttribute('aria-valuenow', String(Math.round(clamped)));
  elements.compare.classList.toggle('hide-before', clamped <= 2);
  elements.compare.classList.toggle('hide-after', clamped >= 98);
}
window.setSplit = setSplit;

function pointerDown(event) {
  if (elements.compare.classList.contains('empty')) return;

  event.preventDefault();

  if (nearDivider(event)) {
    dragging = true;
    elements.compare.setPointerCapture(event.pointerId);
    document.body.classList.add('dragging');
    moveSplit(event);
  }

  if (loupeEnabled) {
    elements.loupe.classList.add('on');
    updateLoupe(event);
  }
}

function pointerMove(event) {
  if (dragging) moveSplit(event);
  updateLoupe(event);
}

function pointerUp(event) {
  endDrag(event);
  if (event.pointerType === 'touch') elements.loupe.classList.remove('on');
}

function pointerCancel(event) {
  endDrag(event);
  elements.loupe.classList.remove('on');
}

function moveSplit(event) {
  const rect = elements.compare.getBoundingClientRect();
  setSplit(((event.clientX - rect.left) / rect.width) * 100);
  window.getSelection()?.removeAllRanges();
}

function nearDivider(event) {
  const rect = elements.compare.getBoundingClientRect();
  const dividerX = rect.left + rect.width * currentSplit / 100;
  const radius = event.pointerType === 'touch' ? 36 : 28;
  return Math.abs(event.clientX - dividerX) <= radius;
}

function endDrag(event) {
  if (dragging && elements.compare.hasPointerCapture(event.pointerId)) elements.compare.releasePointerCapture(event.pointerId);

  dragging = false;
  document.body.classList.remove('dragging');
}

function updateLoupe(event) {
  if (!loupeEnabled || elements.compare.classList.contains('empty') || !outputLoupeUrl) return;

  const rect = elements.compare.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  const sourceUrl = x / rect.width * 100 < currentSplit ? inputLoupeUrl : outputLoupeUrl;
  if (!sourceUrl) return;

  const size = Math.max(elements.loupe.offsetWidth, 190);
  const margin = 8;
  let left = event.clientX + 18;
  let top = event.clientY + 18;

  if (left + size + margin > window.innerWidth) left = event.clientX - size - 18;
  if (top + size + margin > window.innerHeight) top = event.clientY - size - 18;

  left = Math.max(margin, Math.min(window.innerWidth - size - margin, left));
  top = Math.max(margin, Math.min(window.innerHeight - size - margin, top));

  elements.loupe.style.left = `${left}px`;
  elements.loupe.style.top = `${top}px`;
  elements.loupe.style.backgroundImage = `url(${sourceUrl})`;
  elements.loupe.style.backgroundSize = `${rect.width * 5}px ${rect.height * 5}px`;
  elements.loupe.style.backgroundPosition = `${-(x * 5 - 95)}px ${-(y * 5 - 95)}px`;
}

function toggleLoupe() {
  loupeEnabled = !loupeEnabled;
  elements.toggleLoupe.textContent = `Loupe: ${loupeEnabled ? 'on' : 'off'}`;
  elements.loupe.classList.toggle('on', loupeEnabled);
}

function openHelp(key) {
  const item = help[key];
  if (!item) return;

  elements.helpTitle.textContent = item.title;
  elements.helpCopy.textContent = item.copy;
  elements.helpExamples.innerHTML = '';

  for (let index = 0; index < item.values.length; index++) {
    const wrap = document.createElement('div');
    const canvas = document.createElement('canvas');
    const label = document.createElement('strong');

    wrap.className = 'example';
    canvas.width = 220;
    canvas.height = 92;
    label.textContent = item.labels[index];
    wrap.append(canvas, label);
    elements.helpExamples.append(wrap);
    drawHelpExample(canvas, key, item.values[index]);
  }

  elements.helpModal.showModal();
}

function drawHelpExample(canvas, key, value) {
  const context = canvas.getContext('2d');
  const width = canvas.width;
  const height = canvas.height;
  const gradient = context.createLinearGradient(0, 0, width, height);
  gradient.addColorStop(0, '#0f172a');
  gradient.addColorStop(0.42, '#22c55e');
  gradient.addColorStop(0.68, '#c084fc');
  gradient.addColorStop(1, '#fde68a');
  context.fillStyle = gradient;
  context.fillRect(0, 0, width, height);

  const original = context.getImageData(0, 0, width, height);
  const image = new ImageData(new Uint8ClampedArray(original.data), width, height);
  const palette = parseColorscheme(elements.schemeText.value).slice(0, 8).map(hexToRgb).map(rgb => rgb.map(channel => channel * 255));

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const offset = (y * width + x) * 4;
      const luma = (image.data[offset] + image.data[offset + 1] + image.data[offset + 2]) / 765;
      const noise = ((x * 17 + y * 31) % 23) / 22 - 0.5;
      let t = Math.max(0, Math.min(1, luma + (key === 'dither' ? noise * value * 0.26 : 0)));
      if (key === 'radius') t = Math.round(t * (22 - Math.min(value, 45) / 2)) / (22 - Math.min(value, 45) / 2);
      if (key === 'threshold') t = Math.round(t * Math.max(2, value / 2)) / Math.max(2, value / 2);

      const color = palette[Math.min(palette.length - 1, Math.floor(t * palette.length))] || [image.data[offset], image.data[offset + 1], image.data[offset + 2]];
      const blend = key === 'blend' ? value : 0.88;
      image.data[offset] = original.data[offset] * (1 - blend) + color[0] * blend;
      image.data[offset + 1] = original.data[offset + 1] * (1 - blend) + color[1] * blend;
      image.data[offset + 2] = original.data[offset + 2] * (1 - blend) + color[2] * blend;
    }
  }

  context.putImageData(image, 0, 0);
}
async function fileToImageData(file) {
  if ('createImageBitmap' in window) {
    try {
      const bitmap = await withTimeout(createImageBitmap(file), 12_000);
      return bitmapToImageData(bitmap);
    } catch {
    }
  }

  return imageElementToImageData(file);
}

async function imageElementToImageData(file) {
  const url = URL.createObjectURL(file);
  const image = new Image();

  try {
    image.decoding = 'async';
    image.src = url;
    await withTimeout(image.decode(), 12_000);

    return drawableToImageData(image, image.naturalWidth, image.naturalHeight);
  } finally {
    URL.revokeObjectURL(url);
  }
}

function bitmapToImageData(bitmap) {
  try {
    return drawableToImageData(bitmap, bitmap.width, bitmap.height);
  } finally {
    bitmap.close?.();
  }
}

function drawableToImageData(drawable, width, height) {
  if (!width || !height) throw new Error('Could not decode image dimensions.');

  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const context = canvas.getContext('2d', { colorSpace: 'srgb' });
  context.drawImage(drawable, 0, 0);

  return context.getImageData(0, 0, canvas.width, canvas.height);
}

function withTimeout(promise, ms) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error('Image decode timed out. Try a smaller image or another browser.')), ms)),
  ]);
}

function drawImageData(canvas, imageData) {
  canvas.width = imageData.width;
  canvas.height = imageData.height;
  canvas.getContext('2d').putImageData(imageData, 0, 0);
}

async function updateDownload() {
  if (outputDownloadUrl) URL.revokeObjectURL(outputDownloadUrl);

  const blob = await canvasBlob(elements.outputCanvas);
  outputDownloadUrl = URL.createObjectURL(blob);
  elements.download.href = outputDownloadUrl;
  elements.download.download = inputFileName.replace(/\.[^.]*$/, '_colorized.png');
  elements.download.hidden = false;
}

function replaceLoupeUrl(kind, url) {
  if (kind === 'input') inputLoupeUrl = url;
  else outputLoupeUrl = url;
}

function drawHelpStatus(text) {
  setStatus(text);
}

function parseBase16Yaml(text) {
  const colors = new Map();

  for (const line of text.split('\n')) {
    const match = line.trim().match(/^(base[0-9A-Fa-f]{2}):\s*['"]?([0-9A-Fa-f]{6})['"]?/);
    if (match) colors.set(match[1].toLowerCase(), `#${match[2].toLowerCase()}`);
  }

  return [...colors].sort(([left], [right]) => left.localeCompare(right)).map(([, color]) => color);
}

function schemeStripSpans(colors) {
  return colors.slice(0, 16).map(color => `<span style="background:${color}"></span>`).join('');
}

function applyTheme(colors) {
  if (!colors.length) return;

  const sorted = [...colors].sort((left, right) => luminance(left) - luminance(right));
  const root = document.documentElement.style;
  const dark = sorted[0];
  const panel = sorted[Math.min(1, sorted.length - 1)];
  const light = sorted[sorted.length - 1];
  const muted = sorted[Math.max(0, sorted.length - 2)];
  const accents = sorted.slice(2, -2).length ? sorted.slice(2, -2) : sorted;
  const accent = accents[Math.floor(accents.length * 0.62)] || light;
  const green = accents[Math.floor(accents.length * 0.42)] || accent;
  const orange = accents[Math.floor(accents.length * 0.78)] || accent;
  const red = accents[Math.floor(accents.length * 0.9)] || accent;

  root.setProperty('--bg', dark);
  root.setProperty('--panel', panel);
  root.setProperty('--panel-2', sorted[Math.min(2, sorted.length - 1)] || panel);
  root.setProperty('--text', light);
  root.setProperty('--muted', muted);
  root.setProperty('--quiet', sorted[Math.floor(sorted.length * 0.68)] || muted);
  root.setProperty('--blue', accent);
  root.setProperty('--green', green);
  root.setProperty('--orange', orange);
  root.setProperty('--red', red);
  root.setProperty('--line', sorted[Math.floor(sorted.length * 0.32)] || panel);
  root.setProperty('--button-text', luminance(accent) > 0.42 ? dark : light);
}

function luminance(hex) {
  const [r, g, b] = hexToRgb(hex).map(value => {
    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  });

  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function downloadText(name, text, type) {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const link = document.createElement('a');
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

function canvasBlob(canvas) {
  return new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
}

async function fetchText(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} returned ${response.status}`);

  return response.text();
}

function sanitizeName(name) {
  return name.trim().toLowerCase().replace(/[^a-z0-9._-]+/g, '-').replace(/^-|-$/g, '') || 'custom';
}

function setStatus(message) {
  elements.status.className = 'status';
  elements.status.textContent = message;
}

function setError(message) {
  elements.status.className = 'status error';
  elements.status.textContent = message;
}

function escapeHtml(value) {
  return value.replace(/[&<>"]/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[char]));
}
