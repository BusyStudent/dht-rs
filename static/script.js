// --- 常量 ---
const MAX_LOG_LINES = 1000;

// --- 全局变量和初始化 ---
let searchDebounceTimer; // 用于搜索防抖的计时器
let routingTableTimer;   // 用于更新路由表的计时器
let currentQuery = '';   // 当前搜索的关键词
let currentPage = 1;     // 当前搜索的页码

loadSettings();
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

        // 1. 处理功能按钮点击
        const button = target.closest('button[data-action]');
        if (button) {
            const action = button.dataset.action;
            const hash = button.dataset.hash;
            
            if (action === 'download-torrent') {
                window.open(`/api/v1/torrents/${hash}`, '_blank');
            } else if (action === 'copy-magnet') {
                const magnet = `magnet:?xt=urn:btih:${hash}`;
                navigator.clipboard.writeText(magnet).then(() => alert(`磁力链接已复制: ${magnet}`));
            } else if (action === 'toggle-files') {
                const fileList = button.closest('.torrent-card').querySelector('.file-list');
                if (fileList) {
                    // [BUG修复] 改为切换CSS类，而不是直接操作style
                    fileList.classList.toggle('is-shown');
                }
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
}

function setupEventSource() {
    const eventSource = new EventSource('/api/v1/sse/events'); // 连接到 SSE 端点
    eventSource.onmessage = (event) => {
        addLog(event.data);
    };
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

    // console.log(`正在模拟搜索: "${currentQuery}", 第 ${currentPage} 页`);
    // setTimeout(() => {
    //      const mockFullData = {
    //         results: [
    //             {
    //                 name: 'ubuntu-22.04.3-desktop-amd64.iso',
    //                 info_hash: 'f8c0e6e7d4b3a299f1e9a217c5b3f0e8a1c2b3d4',
    //                 size: '4.7 GB',
    //                 files: [{ name: 'ubuntu.iso', size: '4.7 GB' }, { name: 'README.txt', size: '1.2 KB' }]
    //             },
    //             { // 新增：只有 hash 的情况
    //                 info_hash: 'aabbccddeeff00112233445566778899aabbccdd'
    //             },
    //             {
    //                 name: '[Tears of Steel] a short film by the Blender Foundation',
    //                 info_hash: 'd8c0e6e7d4b3a299f1e9a217c5b3f0e8a1c2b3d9',
    //                 size: '522 MB',
    //                 files: [{ name: 'Tears of Steel.mp4', size: '520 MB' }, { name: 'poster.jpg', size: '2 MB' }]
    //             }
    //         ],
    //         totalPages: 5,
    //         currentPage: currentPage
    //     };
    //     displaySearchResults(mockFullData.results);
    //     renderPaginationControls(mockFullData.totalPages, mockFullData.currentPage);
    // }, 800);
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

    results.forEach(item => {
        let cardHTML;
        if (item.name) { // 有完整元数据
            const fileTree = buildFileTree(item.files);
            const fileListHTML = renderTreeToHTML(fileTree);
            // [BUG修复] 移除 file-list 上的内联 style 属性
            cardHTML = `
                <div class="torrent-card">
                    <h4>${item.name}</h4>
                    <p class="info-hash">Hash: ${item.info_hash}</p>
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
                <div class="torrent-card minimal">
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
