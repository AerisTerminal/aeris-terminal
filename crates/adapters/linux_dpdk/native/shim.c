/*
 * Thin C shim over DPDK's header-inline fast-path API.
 *
 * rte_eth_rx_burst, rte_eth_tx_burst, rte_pktmbuf_alloc, rte_pktmbuf_free, and
 * rte_pktmbuf_mtod are static inline functions or macros in the DPDK headers, so
 * no shared-library symbols exist for bindgen to call. This shim re-exports them
 * under stable names; the C compiler resolves the exact inline semantics against
 * the reviewed 25.11.0 headers instead of Rust replicating header internals.
 *
 * Every function here is a one-line forward to the documented DPDK inline with no
 * logic of its own; behavioral invariants are owned by the Rust safe boundary in
 * native_sys.rs.
 */
#include <stdint.h>

#include <rte_ethdev.h>
#include <rte_mbuf.h>
#include <rte_mempool.h>
#include <rte_errno.h>

uint16_t axiusflow_rte_eth_rx_burst(uint16_t port_id, uint16_t queue_id,
		struct rte_mbuf **rx_pkts, uint16_t nb_pkts)
{
	return rte_eth_rx_burst(port_id, queue_id, rx_pkts, nb_pkts);
}

uint16_t axiusflow_rte_eth_tx_burst(uint16_t port_id, uint16_t queue_id,
		struct rte_mbuf **tx_pkts, uint16_t nb_pkts)
{
	return rte_eth_tx_burst(port_id, queue_id, tx_pkts, nb_pkts);
}

struct rte_mbuf *axiusflow_rte_pktmbuf_alloc(struct rte_mempool *mp)
{
	return rte_pktmbuf_alloc(mp);
}

void axiusflow_rte_pktmbuf_free(struct rte_mbuf *m)
{
	rte_pktmbuf_free(m);
}

void *axiusflow_rte_pktmbuf_mtod(struct rte_mbuf *m)
{
	return rte_pktmbuf_mtod(m, void *);
}

uint16_t axiusflow_rte_pktmbuf_tailroom(struct rte_mbuf *m)
{
	return rte_pktmbuf_tailroom(m);
}

void axiusflow_rte_pktmbuf_set_len(struct rte_mbuf *m, uint16_t len)
{
	m->data_len = len;
	m->pkt_len = len;
}

uint16_t axiusflow_rte_pktmbuf_data_len(struct rte_mbuf *m)
{
	return m->data_len;
}

int axiusflow_rte_errno(void)
{
	return rte_errno;
}
