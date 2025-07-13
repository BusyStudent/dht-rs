// --- 常量 ---
const MAX_LOG_LINES = 1000;

// --- 全局变量和初始化 ---
let searchDebounceTimer; // 用于搜索防抖的计时器
let routingTableTimer;   // 用于更新路由表的计时器
let dashboardTimer;      // 用于更新仪表盘的计时器
let currentQuery = '';   // 当前搜索的关键词
let currentPage = 1;     // 当前搜索的页码

// [新增] 模块化仪表盘状态管理
const dashboard = {
    chartInstance: null,
    // 用于生成连续模拟数据
    mockDataHistory: {
        metadataCount: 500, 
    }
};

loadSettings();
initDashboard(); // 初始化仪表盘模块
addLog("欢迎使用 DHT Indexer 控制面板。");
addLog("前端界面已就绪，等待与后端交互。");
setupEventListeners();
setupEventSource();
queryStatus();

// --- Tab 切换逻辑 ---
function showTab(tabName) {
    document.querySelectorAll('.tab-content').forEach(c => c.classList.remove('active'));
    document.querySelectorAll('.tab-button').forEach(b => b.classList.remove('active'));
    document.getElementById(`${tabName}-content`).classList.add('active');
    document.querySelector(`.tab-button[onclick="showTab('${tabName}')"]`).classList.add('active');
    if (routingTableTimer != null) {
        console.log('关闭路由表计时器');
        clearInterval(routingTableTimer);
        routingTableTimer = null;
    }

    if (tabName === 'routing') {
        console.log('启动路由表计时器');
        updateRoutingTable();
        routingTableTimer = setInterval(updateRoutingTable, 5000); // 每隔5s更新一下
    }
}

// --- 事件监听器设置 ---
function setupEventListeners() {
    // 启动/停止
    document.getElementById('start-btn').addEventListener('click', () => {
        startCrawler();
    });
    document.getElementById('stop-btn').addEventListener('click', () => {
        stopCrawler();
    });
    // 主动采样
    document.getElementById('active-sample-btn').addEventListener('click', () => {
        setActiveSample();
    })

    // 搜索框输入（防抖）
    document.getElementById('search-input').addEventListener('input', (e) => {
        clearTimeout(searchDebounceTimer);
        const query = e.target.value.trim();
        searchDebounceTimer = setTimeout(() => {
            if (query !== currentQuery) {
                currentQuery = query;
                searchMetadata(1); // 新的搜索总是从第一页开始
            }
        }, 250); // 停止输入250ms后触发
    });

    // 保存设置
    document.getElementById('save-settings-btn').addEventListener('click', saveSettings);

    // 搜索结果区域的事件委托 (处理按钮点击和文件树展开/折叠)
    document.getElementById('search-results').addEventListener('click', (e) => {
        const target = e.target;

        // 处理标签删除
        if (target.classList.contains('tag-delete-btn')) {
            const hash = target.dataset.hash;
            const tag = target.dataset.tag;
            handleTagRemove(hash, tag, target);
            return;
        }

        // 1. 处理功能按钮点击
        const button = target.closest('button[data-action]');
        if (button) {
            const action = button.dataset.action;
            const card = button.closest('.torrent-card');
            const hash = card ? card.dataset.hash : null;

            if (!hash) return; // Should not happen if button is inside a card
            
            if (action === 'download-torrent') {
                window.open(`/api/v1/torrents/${hash}`, '_blank');
            } else if (action === 'copy-magnet') {
                const magnet = `magnet:?xt=urn:btih:${hash}`;
                navigator.clipboard.writeText(magnet).then(() => alert(`磁力链接已复制: ${magnet}`));
            } else if (action === 'toggle-files') {
                const fileList = card.querySelector('.file-list');
                if (fileList) {
                    fileList.classList.toggle('is-shown');
                }
            } else if (action === 'add-tag') {
                showAddTagModal(hash);
            }
            return; // 按钮事件已处理，终止
        }

        // 2. 处理文件树文件夹点击
        const folderToggle = target.closest('.tree-folder');
        if (folderToggle) {
            folderToggle.parentElement.classList.toggle('expanded');
        }
    });


    // 翻页控件的事件委托
    document.getElementById('pagination-controls').addEventListener('click', (e) => {
        e.preventDefault();
        const target = e.target.closest('.pagination-link');
        if (!target || target.classList.contains('disabled') || target.classList.contains('active')) {
            return;
        }
        const page = parseInt(target.dataset.page, 10);
        searchMetadata(page);
    });

    // 手动工具区域的事件委托
    document.getElementById('tools-content').addEventListener('click', (e) => {
        const target = e.target;

        // 处理 "随机" 按钮
        if (target.classList.contains('random-btn')) {
            const targetInputId = target.dataset.randomFor;
            if (targetInputId) {
                const inputElement = document.getElementById(targetInputId);
                if (inputElement) {
                    // Node ID 和 Info Hash 都是 20 字节
                    inputElement.value = generateRandomHex(20);
                }
            }
        }

        // 处理 "发送" 按钮
        if (target.classList.contains('tool-btn')) {
            const toolName = target.dataset.tool;
            handleToolRequest(toolName);
        }
    });

    // --- [新增] 模态窗口事件监听 ---
    const modal = document.getElementById('add-tag-modal');
    document.getElementById('modal-cancel-btn').addEventListener('click', hideAddTagModal);
    modal.addEventListener('click', (e) => {
        if (e.target === modal) { // 点击背景关闭
            hideAddTagModal();
        }
    });
    document.getElementById('modal-add-btn').addEventListener('click', () => {
        const hash = document.getElementById('modal-add-btn').dataset.hash;
        const input = document.getElementById('modal-tag-input');
        const tag = input.value.trim();
        if (tag && hash) {
            handleTagAdd(hash, tag);
        }
    });
}

// --- [新增] 标签模态窗口函数 ---
function showAddTagModal(hash) {
    const modal = document.getElementById('add-tag-modal');
    document.getElementById('modal-info-hash-display').textContent = hash;
    document.getElementById('modal-add-btn').dataset.hash = hash;
    modal.style.display = 'flex';
    document.getElementById('modal-tag-input').focus();
}

function hideAddTagModal() {
    const modal = document.getElementById('add-tag-modal');
    document.getElementById('modal-tag-input').value = '';
    modal.style.display = 'none';
}

async function handleTagAdd(hash, tag) {
    try {
        const response = await fetch(`/api/v1/metadata/${hash}/tags`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ tag: tag })
        });
        if (!response.ok) {
            const error = await response.json();
            throw new Error(error.error || 'Failed to add tag');
        }
        
        // UI update
        const card = document.querySelector(`.torrent-card[data-hash="${hash}"]`);
        if (card) {
            const tagsContainer = card.querySelector('.tags-container');
            const newTagHTML = `
                <span class="tag-item">
                    ${tag.replace(/</g, "&lt;").replace(/>/g, "&gt;")}
                    <button class="tag-delete-btn" data-hash="${hash}" data-tag="${tag}">&times;</button>
                </span>
            `;
            tagsContainer.innerHTML += newTagHTML;
        }
        hideAddTagModal();
    } catch (err) {
        alert(`Error: ${err.message}`);
    }
}

async function handleTagRemove(hash, tag, buttonElement) {
    try {
        const response = await fetch(`/api/v1/metadata/${hash}/tags`, {
            method: 'DELETE',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ tag: tag })
        });
        if (!response.ok) {
            const error = await response.json();
            throw new Error(error.error || 'Failed to remove tag');
        }
        
        // UI update
        buttonElement.parentElement.remove();

    } catch (err) {
        alert(`Error: ${err.message}`);
    }
}

function setupEventSource() {
    const eventSource = new EventSource('/api/v1/sse/events'); // 连接到 SSE 端点
    eventSource.onmessage = (event) => {
        try {
            processEvent(event);
        }
        catch (error) {
            console.log('Error parsing SSE message:', error);
        }
    };
}

// --- [重构] 仪表盘模块 ---

/**
 * 初始化仪表盘，包括创建图表
 * 这是模块化的一部分，将所有仪表盘相关的初始化放在一起
 */
function initDashboard() {
    const ctx = document.getElementById('metadata-chart').getContext('2d');
    
    dashboard.chartInstance = new Chart(ctx, {
        type: 'line',
        data: {
            labels: [], // X轴，时间
            datasets: [{
                label: '抓取的元数据数量',
                data: [], // Y轴，数量
                backgroundColor: 'rgba(52, 152, 219, 0.2)',
                borderColor: 'rgba(52, 152, 219, 1)',
                borderWidth: 2,
                fill: true,
                tension: 0.4 // 使线条平滑
            }]
        },
        options: {
            responsive: true,
            maintainAspectRatio: false,
            scales: {
                y: {
                    beginAtZero: false // Y轴不一定从0开始，更好地显示变化
                },
                x: {
                    ticks: {
                        maxRotation: 0,
                        minRotation: 0,
                        autoSkip: true,
                        maxTicksLimit: 10 // X轴最多显示10个标签，防止拥挤
                    }
                }
            },
            plugins: {
                legend: {
                    display: false // 只有一个数据集，可以隐藏图例
                }
            }
        }
    });
}


/**
 * 更新仪表盘的 UI 显示 (卡片和图表)
 * @param {object} data - 包含仪表盘数据的对象
 * @example updateDashboardUI({ nodes: 1234, infohashes: 5678, metadata: 910, peers: 112 })
 */
function updateDashboardUI(data) {
    // 1. 更新统计卡片
    document.getElementById('stat-nodes').textContent = data.nodes;
    document.getElementById('stat-infohashes').textContent = data.infohashes;
    document.getElementById('stat-metadata').textContent = data.metadata;
    document.getElementById('stat-peers').textContent = data.peers;
    document.getElementById('stat-fetching').textContent = data.fetching;

    // 2. 更新图表
    const chart = dashboard.chartInstance;
    const now = new Date().toLocaleTimeString();

    // 添加新数据
    chart.data.labels.push(now);
    chart.data.datasets[0].data.push(data.metadata);

    // 限制图表显示的数据点数量，防止无限增长
    const maxDataPoints = 30; 
    if (chart.data.labels.length > maxDataPoints) {
        chart.data.labels.shift(); // 移除最旧的标签
        chart.data.datasets[0].data.shift(); // 移除最旧的数据
    }

    // 调用 chart.js 的 update 方法来重绘图表
    chart.update();
}

// --- 与后端交互的函数 ---
function updateStartButtonStates(isRunning) {
    const startBtn = document.getElementById('start-btn');
    const stopBtn = document.getElementById('stop-btn');
    const activeSampleBtn = document.getElementById('active-sample-btn');

    if (isRunning) {
        startBtn.style.display = 'none';
        stopBtn.style.display = 'inline-block';
        activeSampleBtn.style.display = 'inline-block';
    }
    else {
        startBtn.style.display = 'inline-block';
        stopBtn.style.display = 'none';
        activeSampleBtn.style.display = 'none';
    }
}
function updateSampleButtonStatus(enable) {
    const activeSampleBtn = document.getElementById('active-sample-btn');
    if (enable) {
        activeSampleBtn.textContent = '关闭主动采样';
        activeSampleBtn.className = 'button danger';
    }
    else {
        activeSampleBtn.textContent = '开启主动采样';
        activeSampleBtn.className = 'button';
    }
}

// 看看有没有已经启动了
async function queryStatus() {
    try {
        const response = await fetch('/api/v1/status');
        const status = await response.json();

        updateStartButtonStates(status['running']);
        if (status["running"]) {
            // 设置额外字段
            updateSampleButtonStatus(status['auto_sample']);
        }
    }
    catch (e) {
        // ignore
    }
}

async function startCrawler() {
    if (document.getElementById('start-btn').style.display === 'none') {
        return; // 早就有个任务在启动了
    }
    addLog("正在尝试启动爬虫...");
    try {
        const response = await fetch('/api/v1/start_dht');
        if (!response.ok) {
            addLog(`不能连接到后端 因为 ${response.statusText}`);
            return;
        }
        const data = await response.text();
        addLog(`后端返回: ${data}`);

        // 显示停止按钮，隐藏开始按钮
        updateStartButtonStates(true);
    }
    catch (e) {
        console.log(e);
    }
}

async function stopCrawler() {
    if (document.getElementById('stop-btn').style.display === 'none') {
        return; // 早就有个任务在停止了
    }
    addLog("正在发送停止请求...");
    try {
        const response = await fetch('/api/v1/stop_dht');
        if (!response.ok) {
            addLog(`不能连接到后端 因为 ${response.statusText}`);
            return;
        }
        const data = await response.text();
        addLog(`后端返回: ${data}`);

        // 显示开始按钮，隐藏停止按钮
        updateStartButtonStates(false);
    }
    catch (e) {
        console.log(e);
    }
}

async function setActiveSample() {
    const btn = document.getElementById('active-sample-btn');
    let enabled = btn.className == 'button danger'; // 是不是已经启动了？
    enabled = !enabled;

    try {
        const response = await fetch('/api/v1/set/auto_sample', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify(enabled)
        });
        const json = await response.json();
        if (json['error']) {
            throw new Error(json['error']);
        }
        const current = json['current'];
        updateSampleButtonStatus(current);
    }
    catch (e) {
        console.log(e);
    }
}

function addLog(message) {
    const logWindow = document.getElementById('log-window');
    const timestamp = new Date().toLocaleTimeString();

    // 检查添加日志前，窗口是否已经滚动到底部
    // 增加 1px 的容差，以应对不同浏览器的计算差异
    const isScrolledToBottom = logWindow.scrollHeight - logWindow.clientHeight <= logWindow.scrollTop + 1;

    // 使用 textContent 追加新日志，这比 innerHTML 更安全、性能更好
    logWindow.textContent += `[${timestamp}] ${message}\n`;
    
    const lines = logWindow.textContent.split('\n');
    if (lines.length > MAX_LOG_LINES + 1) {
        // 太多日志
        const newText = lines.slice(-MAX_LOG_LINES).join('\n');
        logWindow.textContent = newText;
    }

    // 只有在添加日志前视图就在底部时，才自动滚动到底部
    if (isScrolledToBottom) {
        logWindow.scrollTop = logWindow.scrollHeight;
    }
}

function processEvent(event) {
    const json = JSON.parse(event.data);
    switch (json['type']) {
        case 'log': 
            addLog(json['data']);
            break;
        case 'downloaded':
            const hash = json['data']['hash'];
            const name = json['data']['name'];
            addLog(`下载到元数据 ${name}`);
            break;
        case 'dashboard':
            updateDashboardUI(json['data']);
            break;
        default:
            console.log('Unknown event type:', json['type']);
            break
    }
}

async function updateRoutingTable() {
    const list = document.getElementById('routing-table-list');
    const countSpan = document.getElementById('node-count');
    try {
        // list.innerHTML = '<li>正在从后端获取数据...</li>';
        // setTimeout(() => {
        //     const mockNodes = ['a1b2c3d4... | 192.168.1.10:6881','e5f6g7h8... | 8.8.8.8:6881','i9j0k1l2... | 202.108.22.5:12345'];
        //     list.innerHTML = mockNodes.map(node => `<li>${node}</li>`).join('');
        //     countSpan.textContent = mockNodes.length;
        // }, 1000);
        const response = await fetch('/api/v1/get_routing_table');
        if (!response.ok) {
            list.innerHTML = '<li>无法连接到后端</li>';
            return;
        }
        const json = await response.json();
        list.innerHTML = json.map(node => `<li>${node.id} | ${node.ip}</li>`).join('');
        countSpan.textContent = json.length;
    }
    catch (e) {
        list.innerHTML = `<li>出错 ${e}</li>`;
    }
    
}

/**
 * 元数据查找: 执行搜索 (支持翻页)
 * @param {number} page - 要请求的页码
 */
async function searchMetadata(page = 1) {
    currentPage = page;
    const resultsContainer = document.getElementById('search-results');

    if (!currentQuery) {
        resultsContainer.innerHTML = '';
        renderPaginationControls(0, 0);
        return;
    }

    // resultsContainer.innerHTML = '<p>正在搜索, 请稍候...</p>';
    renderPaginationControls(0, 0); // 清空旧的翻页

    try {
        const response = await fetch('/api/v1/search_metadata', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ query: currentQuery, page: currentPage })
        });
        if (!response.ok) {
                throw new Error(`HTTP error! status: ${response.status}`);
        }
        const json = await response.json();
        const error = json.error;
        if (error != null) {
            throw new Error(error);
        }
        displaySearchResults(json.results);
        renderPaginationControls(json.totalPages, json.currentPage);
    }
    catch (e) {
        resultsContainer.innerHTML = `<p>搜索失败: ${e}</p>`;
    }
}

/**
 * 将扁平的文件列表（包含路径）转换为嵌套的树对象。
 * @param {Array<Object>} files - 例如 [{ name: 'dir/file.txt', size: '1KB' }]
 * @returns {Object} 树状结构
 */
function buildFileTree(files) {
    const tree = {};
    files.forEach(file => {
        const pathParts = file.name.split('/');
        let currentLevel = tree;

        pathParts.forEach((part, index) => {
            if (index === pathParts.length - 1) { // 路径的最后一部分是文件
                currentLevel[part] = { type: 'file', size: file.size };
            } 
            else { // 路径的中间部分是文件夹
                if (!currentLevel[part]) {
                    currentLevel[part] = { type: 'folder', children: {} };
                }
                currentLevel = currentLevel[part].children;
            }
        });
    });
    return tree;
}

/**
 * 将文件树对象递归地渲染成HTML字符串。
 * @param {Object} treeNode - 文件树的一个节点
 * @returns {string} HTML字符串
 */
function renderTreeToHTML(treeNode) {
    // 排序，使文件夹总是在文件前面
    const sortedKeys = Object.keys(treeNode).sort((a, b) => {
        const itemA = treeNode[a];
        const itemB = treeNode[b];
        const aIsFolder = itemA.type === 'folder';
        const bIsFolder = itemB.type === 'folder';

        if (aIsFolder && !bIsFolder) return -1;
        if (!aIsFolder && bIsFolder) return 1;
        return a.localeCompare(b); // 同类型则按字母排序
    });

    let html = '<ul>';
    for (const key of sortedKeys) {
        const item = treeNode[key];
        if (item.type === 'folder') {
            html += `<li class="tree-folder-container">
                        <span class="tree-item tree-folder">${key}</span>
                        ${renderTreeToHTML(item.children)}
                        </li>`;
        } else {
            html += `<li>
                        <span class="tree-item tree-file">${key}</span>
                        <span class="file-size">(${item.size})</span>
                        </li>`;
        }
    }
    html += '</ul>';
    return html;
}

/**
 * 元数据查找: 显示搜索结果 (适配纯hash和树状文件列表)
 * @param {Array} results - 搜索结果数组
 */
function displaySearchResults(results) {
    const container = document.getElementById('search-results');
    container.innerHTML = '';

    if (!results || results.length === 0) {
        container.innerHTML = '<p>没有找到相关结果。</p>';
        return;
    }
    const addTagButtonHTML = `<button class="button add-tag-button" data-action="add-tag"></button>`;
    results.forEach(item => {
        const tagsHTML = (item.tags || []).map(tag => `
            <span class="tag-item">
                ${tag.replace(/</g, "&lt;").replace(/>/g, "&gt;")}
                <button class="tag-delete-btn" data-hash="${item.info_hash}" data-tag="${tag}">&times;</button>
            </span>
        `).join('');

        let cardHTML;
        if (item.name) { // 有完整元数据
            const fileTree = buildFileTree(item.files);
            const fileListHTML = renderTreeToHTML(fileTree);
            // [BUG修复] 移除 file-list 上的内联 style 属性
            cardHTML = `
                <div class="torrent-card" data-hash="${item.info_hash}">
                    <h4>${item.name}</h4>
                    <p class="info-hash">Hash: ${item.info_hash}</p>
                    <div class="tags-container">${tagsHTML}${addTagButtonHTML}</div>
                    <div class="actions">
                        <button class="button" data-action="download-torrent" data-hash="${item.info_hash}">下载种子</button>
                        <button class="button" data-action="copy-magnet" data-hash="${item.info_hash}">复制磁力</button>
                        <button class="button" data-action="toggle-files">查看文件</button>
                    </div>
                    <div class="file-list">${fileListHTML}</div>
                </div>
            `;
        } else { // 只有 info_hash
            cardHTML = `
                <div class="torrent-card minimal" data-hash="${item.info_hash}">
                    <h4>元数据待获取</h4>
                    <p class="info-hash">Hash: ${item.info_hash}</p>
                    <div class="actions">
                        <button class="button" data-action="download-torrent" data-hash="${item.info_hash}">下载种子</button>
                        <button class="button" data-action="copy-magnet" data-hash="${item.info_hash}">复制磁力</button>
                    </div>
                </div>
            `;
        }
        container.innerHTML += cardHTML;
    });
}

/**
 * 渲染翻页控件
 * @param {number} totalPages - 总页数
 * @param {number} currentPage - 当前页码
 */
function renderPaginationControls(totalPages, currentPage) {
    const container = document.getElementById('pagination-controls');
    if (totalPages <= 1) {
        container.innerHTML = '';
        return;
    }

    let html = '';
    // 上一页
    html += `<a href="#" class="pagination-link ${currentPage === 1 ? 'disabled' : ''}" data-page="${currentPage - 1}">上一页</a>`;

    // 页码
    // 简单逻辑：只显示当前页前后几页
    const startPage = Math.max(1, currentPage - 2);
    const endPage = Math.min(totalPages, currentPage + 2);

    if (startPage > 1) html += `<a href="#" class="pagination-link" data-page="1">1</a>`;
    if (startPage > 2) html += `<span class="pagination-link disabled">...</span>`;

    for (let i = startPage; i <= endPage; i++) {
        html += `<a href="#" class="pagination-link ${i === currentPage ? 'active' : ''}" data-page="${i}">${i}</a>`;
    }

    if (endPage < totalPages - 1) html += `<span class="pagination-link disabled">...</span>`;
    if (endPage < totalPages) html += `<a href="#" class="pagination-link" data-page="${totalPages}">${totalPages}</a>`;

    // 下一页
    html += `<a href="#" class="pagination-link ${currentPage === totalPages ? 'disabled' : ''}" data-page="${currentPage + 1}">下一页</a>`;
    
    container.innerHTML = html;
}

// --- [新增] 工具相关函数 ---

/**
 * 生成指定长度的随机十六进制字符串.
 * @param {number} numBytes - 字节数 (例如, 20 for InfoHash/NodeID).
 * @returns {string} 2 * numBytes 长度的十六进制字符串.
 */
function generateRandomHex(numBytes) {
    const buffer = new Uint8Array(numBytes);
    window.crypto.getRandomValues(buffer);
    return Array.from(buffer, byte => byte.toString(16).padStart(2, '0')).join('');
}

/**
 * 处理手动工具的请求.
 * @param {string} toolName - 工具名称, e.g., 'ping', 'get_peers'.
 */
async function handleToolRequest(toolName) {
    // 将 get_peers 转换为 getpeers 以匹配元素ID
    const responseEl = document.getElementById(`tools-${toolName.replace('_', '-')}-response`);
    if (!responseEl) return;

    responseEl.textContent = '正在发送请求...';

    const endpoint = `/api/v1/tools/${toolName}`;
    let payload = {};

    try {
        // 根据工具名称构建 payload
        switch (toolName) {
            case 'ping':
                payload.address = document.getElementById('tools-ping-addr-input').value;
                if (!payload.address) throw new Error('节点地址不能为空');
                break;
            case 'get_peers':
                // payload.address = document.getElementById('tools-getpeers-addr-input').value;
                payload.info_hash = document.getElementById('tools-getpeers-hash-input').value;
                // if (!payload.address || !payload.info_hash) throw new Error('地址和 Info Hash 不能为空');
                break;
            case 'announce_peer':
                payload.address = document.getElementById('tools-announce-addr-input').value;
                payload.info_hash = document.getElementById('tools-announce-hash-input').value;
                const port = document.getElementById('tools-announce-port-input').value;
                if (!payload.address || !payload.info_hash || !port) throw new Error('地址、Info Hash 和端口不能为空');
                payload.port = parseInt(port, 10);
                break;
            case 'sample_infohashes':
                payload.address = document.getElementById('tools-sample-addr-input').value;
                payload.target = document.getElementById('tools-sample-target-input').value;
                if (!payload.address || !payload.target) throw new Error('地址和目标 Node ID 不能为空');
                break;
            case 'add_hash':
                payload.info_hash = document.getElementById('tools-add-hash-input').value;
                break;
            default:
                throw new Error(`未知的工具: ${toolName}`);
        }

        const response = await fetch(endpoint, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(payload)
        });

        const responseData = await response.json();

        if (!response.ok) {
            // 如果后端返回了带 error 字段的 JSON，就显示它，否则显示通用HTTP错误
            throw new Error(responseData.error || `HTTP 错误: ${response.status}`);
        }

        // 成功，格式化并显示 JSON
        responseEl.textContent = JSON.stringify(responseData, null, 2);

    } catch (err) {
        responseEl.textContent = `错误: ${err.message}`;
    }
}


// ... (loadSettings, saveSettings 函数与之前版本相同)
async function loadSettings() {
    try {
        const response = await fetch('/api/v1/get_config');
        if (!response.ok) {
            console.log('Error:', response.statusText);
            return;
        }
        const config = await response.json();
        const trackers = config['trackers'].join('\n');

        document.getElementById('webui-addr-input').value = config['webui_addr'];
        document.getElementById('bind-addr-input').value = config['bind_addr'];
        document.getElementById('node-id-input').value = config['node_id'];
        document.getElementById('hash-lru-input').value = config['hash_lru'];
        document.getElementById('trackers-input').value = trackers;
    }
    catch (e) {
        console.log(e);
    }
}
async function saveSettings() {
    addLog("正在保存设置...");
    const webui_addr = document.getElementById('webui-addr-input').value;
    const addr = document.getElementById('bind-addr-input').value;
    const id = document.getElementById('node-id-input').value;
    const hash_lru = document.getElementById('hash-lru-input').value;
    const trackers = document.getElementById('trackers-input').value.split('\n').filter((url) => url.length != 0);

    try {
        const response = await fetch('/api/v1/set_config', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify({ 
                webui_addr: webui_addr, 
                bind_addr: addr, 
                node_id: id,
                hash_lru: parseInt(hash_lru, 10),
                trackers: trackers,
            })
        });
        const text = await response.text();

        if (!response.ok) {
            alert(`Error: ${text}`);
            return;
        }
        if (!response.text === "OK") {
            alert(`Error: ${text}`);
            return;
        }

        addLog("设置保存成功");
    }
    catch (e) {
        console.log(e);
    }
}
