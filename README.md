# 一坨DHT爬虫

## 简介

这是一个基于R门的DHT爬虫，爬取DHT网络中的hash，并存储到数据库中。

## 功能

- 爬取DHT网络中的hash
- 抓取并存储元数据到数据库中
- 简单的WebUI用于控制
- IPV4和IPV6都支持 (但是只有一个)

## 已实现

- BEP-3 基础Bittorrent协议和拓展协议
- BEP-5 DHT网络
- BEP-9 传输原数据 (部分, 只有主动获取)
- BEP-29 UTP
- BEP-51 (sample_infohashes)

## TODO

- Trackers 实现更快的元数据获取
- BEP-45 正确实现多地址上的DHT
- IPV4 IPV6双栈DHT
- GetPeers 优化

## 使用方法

``bash
cargo run
``

然后打开WebUI 启动就OK了
