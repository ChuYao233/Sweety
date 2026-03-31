#!/bin/bash
# Sweety Web Server —— Linux 内核参数调优脚本
# 对标 Nginx 生产调优 + TCP BBR + QUIC 优化
# 用法: sudo bash scripts/sysctl_tune.sh
# 撤销: sudo bash scripts/sysctl_tune.sh restore

set -euo pipefail

BACKUP="/etc/sysctl.d/sweety-tune.conf.bak"
CONF="/etc/sysctl.d/99-sweety.conf"

restore() {
    if [[ -f "$BACKUP" ]]; then
        cp "$BACKUP" "$CONF"
        sysctl -p "$CONF"
        echo "已恢复备份"
    else
        rm -f "$CONF"
        echo "无备份，已删除调优配置"
    fi
}

[[ "${1:-}" == "restore" ]] && { restore; exit 0; }

[[ $EUID -ne 0 ]] && { echo "需要 root 权限"; exit 1; }

[[ -f "$CONF" ]] && cp "$CONF" "$BACKUP"

cat > "$CONF" << 'EOF'
# ── TCP 缓冲区（对标 Nginx worker_rlimit_nofile） ──────────────────────────
# 最小/默认/最大 接收/发送缓冲区（字节）
# 最大 128MB：大文件下载时内核有足够缓冲，避免 TCP 窗口缩小
net.core.rmem_max                = 134217728
net.core.wmem_max                = 134217728
net.core.rmem_default            = 262144
net.core.wmem_default            = 262144
net.ipv4.tcp_rmem                = 4096 262144 134217728
net.ipv4.tcp_wmem                = 4096 262144 134217728

# ── TCP 连接队列（对标 Nginx backlog 65535） ────────────────────────────────
net.core.somaxconn               = 65535
net.core.netdev_max_backlog      = 65535
net.ipv4.tcp_max_syn_backlog     = 65535

# ── TCP 连接复用（TIME_WAIT 快速回收） ─────────────────────────────────────
net.ipv4.tcp_tw_reuse            = 1
net.ipv4.tcp_fin_timeout         = 15
net.ipv4.tcp_keepalive_time      = 75
net.ipv4.tcp_keepalive_intvl     = 15
net.ipv4.tcp_keepalive_probes    = 5

# ── TCP 拥塞控制（BBR v2，比 CUBIC 在高带宽高延迟下快 2-10 倍） ────────────
net.core.default_qdisc           = fq
net.ipv4.tcp_congestion_control  = bbr

# ── TCP 快速打开（TFO，减少 1-RTT 建连延迟） ──────────────────────────────
# 3 = 同时启用客户端和服务端 TFO
net.ipv4.tcp_fastopen            = 3

# ── 文件描述符上限 ─────────────────────────────────────────────────────────
fs.file-max                      = 2097152

# ── UDP 缓冲区（QUIC/HTTP3 依赖 UDP） ─────────────────────────────────────
net.core.optmem_max              = 65536

# ── 虚拟内存（减少换页，提升内存密集型负载性能） ──────────────────────────
vm.swappiness                    = 10
vm.dirty_ratio                   = 40
vm.dirty_background_ratio        = 10
EOF

sysctl -p "$CONF"

# ── nofile 限制（需写入 limits.conf） ─────────────────────────────────────
LIMITS="/etc/security/limits.d/99-sweety.conf"
cat > "$LIMITS" << 'EOF'
*    soft nofile 1048576
*    hard nofile 1048576
root soft nofile 1048576
root hard nofile 1048576
EOF

echo ""
echo "✓ 内核参数调优完成"
echo "✓ 文件描述符上限已设为 1048576"
echo ""
echo "验证 BBR 是否生效:"
echo "  sysctl net.ipv4.tcp_congestion_control"
echo ""
echo "验证 TFO:"
echo "  sysctl net.ipv4.tcp_fastopen"
echo ""
echo "撤销: sudo bash scripts/sysctl_tune.sh restore"
