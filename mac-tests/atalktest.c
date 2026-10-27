/*
 * ATalkTest: exercises the AppleTalk link and DDP layer of the built-in
 * AppleTalk node in Snow.
 *
 * Opens DDP socket 0x64 and sends broadcast test datagrams to socket 0x64
 * with DDP type 0x7F and the "SNT1" magic. The AppleTalk node in Snow
 * echoes them; the program checks the echoes and writes the result to
 * "ATalkTest Results" next to itself, in addition to printing it.
 *
 * AppleTalk must be active on the printer port (the default in System 7).
 */
#include <stdint.h>
#include <stdbool.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <OSUtils.h>
#include <Devices.h>
#include <Files.h>

#define openSkt       248
#define closeSkt      247
#define writeDDP      246

/* .ATP driver csCodes (Inside Macintosh: Networking, ch. 6) */
#define nSendRequest  248
#define closeATPSkt   250
#define openATPSkt    254

/* ATP flags */
#define atpXOvalue    32

#define TEST_SOCKET   0x64
#define TEST_TYPE     0x7F
#define TEST_MAGIC    "SNT1"

#define TEST_PACKETS  3

#pragma parameter __D0 PBControl(__D0, __A0, __D1)
pascal OSErr PBControl(short refNum, void *paramBlock, Boolean async) M68K_INLINE(0xA004);

/* .MPP parameter block for the DDP calls (PBControl on .MPP) */
struct mpp_pb {
    void *qLink;            /* +0  */
    short qType;            /* +4  */
    short ioTrap;           /* +6  */
    void *ioCmdAddr;        /* +8  */
    void *ioCompletion;     /* +12 */
    short ioResult;         /* +16 */
    long userData;          /* +18 */
    short reqTID;           /* +22 */
    short ioRefNum;         /* +24 */
    short csCode;           /* +26 */
    unsigned char socket;   /* +28 */
    unsigned char checksumFlag; /* +29 */
    void *listener;         /* +30 (POpenSkt: listener, PWriteDDP: wds ptr) */
};

_Static_assert(sizeof(struct mpp_pb) == 34, "MPP parameter block size");

/* Socket listener (atalk_listener.s) */
extern void atalk_listener(void);
extern void atalk_listener_init(void *buf, volatile short *flag,
                                volatile short *len, volatile short *socket);

static unsigned char rx_buf[600];
static volatile short rx_flag, rx_len, rx_socket;
static struct mpp_pb pb;
static struct atp_pb atp_pb;
static struct bds_elem atp_bds;
static short atp_ref;
static unsigned char atp_socket;
/* The reply's 4-byte user bytes and its data length */
static unsigned char rsp_user[4];
static short rsp_len;
/* The ASP sequence number of the next command */
static unsigned short asp_seq;

/* ATP buffer data structure (one response buffer) */
struct bds_elem {
    short buffSize;             /* +0 */
    unsigned char *buffPtr;     /* +2 */
    short dataSize;             /* +6 */
    long userBytes;             /* +8 */
};

_Static_assert(sizeof(struct bds_elem) == 12, "BDS element size");

/* Multi-packet response buffers: 4 x 578 bytes, consecutive slices of
 * one contiguous reply buffer (the layout the workstation's ASP builds
 * with BuildBDS: each element holds one response packet's data) */
#define MP_NBUF 4
#define MP_BUF_SIZE 578
static struct bds_elem mp_bds[MP_NBUF];
static unsigned char mp_bufs[MP_NBUF][MP_BUF_SIZE];
/* The bytes the .ATP driver put into each buffer */
static short mp_fill[MP_NBUF];
static unsigned char mp_resps;
/* The expected content of big.txt: "0123456789" repeated */
static unsigned char big_expect[1500];
/* The node's address learned from the NBP lookup */
static unsigned char server_node, server_socket;

/* DDP write-data structure (14 bytes) and its 17-byte header block. The
 * header must begin on an odd address, so keep the buffer even-aligned. */
static unsigned char wds_hdr[17] __attribute__((aligned(2))); /* +0 unused */
static struct {
    short pad;               /* +0  */
    unsigned char *header;   /* +2  */
    short data_len;          /* +6  */
    unsigned char *data;     /* +8  */
    short end;               /* +12 (0) */
} wds;

_Static_assert(sizeof(wds) == 14, "DDP write-data structure size");

/* .ATP parameter block (PBControl on .ATP), SendRequest variant */
struct atp_pb {
    void *qLink;                /* +0  */
    short qType;                /* +4  */
    short ioTrap;               /* +6  */
    void *ioCmdAddr;            /* +8  */
    void *ioCompletion;         /* +12 */
    short ioResult;             /* +16 */
    long userData;              /* +18 ATP user bytes */
    short reqTID;               /* +22 */
    short ioRefNum;             /* +24 */
    short csCode;               /* +26 */
    unsigned char atpSocket;    /* +28 */
    unsigned char atpFlags;     /* +29 */
    unsigned char addrBlock[4]; /* +30 net(2) node(1) socket(1) */
    short reqLength;            /* +34 */
    void *reqPointer;           /* +36 */
    void *bdsPointer;           /* +40 */
    unsigned char numOfBuffs;   /* +44 */
    unsigned char timeOutVal;   /* +45 */
    unsigned char numOfResps;   /* +46 */
    unsigned char retryCount;   /* +47 */
    short intBuff;              /* +48 */
    unsigned char TRelTime;     /* +50 */
};

_Static_assert(sizeof(struct atp_pb) == 52, "ATP parameter block size");

static void write_results(const char *text)
{
    const unsigned char name[] = "\pATalkTest Results";
    short refNum;
    long len = (long)strlen(text);
    FSDelete(name, 0);
    if (Create(name, 0, 'ttxt', 'TEXT') != noErr || FSOpen(name, 0, &refNum) != noErr)
        printf("Could not create the results file\n");
    else {
        FSWrite(refNum, &len, text);
        FSClose(refNum);
        FlushVol(NULL, 0);
    }
}

static void progress(const char *text)
{
    printf("%s", text);
    write_results(text);
}

/* Append a diagnostic line to "ATalkTest Log" next to the program */
static void log_line(const char *fmt, ...)
{
    static const unsigned char name[] = "\pATalkTest Log";
    char buf[256];
    va_list ap;
    long len;
    short refNum;

    va_start(ap, fmt);
    len = vsnprintf(buf, sizeof(buf) - 2, fmt, ap);
    va_end(ap);
    if (len < 0)
        return;
    buf[len] = '\n';
    len++;
    if (FSOpen((ConstStringPtr)name, 0, &refNum) != noErr) {
        if (Create((ConstStringPtr)name, 0, 'ttxt', 'TEXT') != noErr
            || FSOpen((ConstStringPtr)name, 0, &refNum) != noErr)
            return;
    }
    SetFPos(refNum, fsFromLEOF, 0);
    FSWrite(refNum, &len, buf);
    FSClose(refNum);
}

/* Send one ASP request as a real ATP transaction (PNSendRequest on the
 * .ATP driver) and wait for the reply synchronously. The ASP user bytes
 * travel in the ATP user data field; the reply's user bytes land in
 * rsp_user and its data in rx_buf (rx_len bytes). The guest's own ATP
 * layer validates the response, so a malformed response shows up as a
 * retry here.
 * Returns 1 if a reply was received, 0 on timeout, -1 on PBControl error. */
static int asp_exchange(short atp_ref, const unsigned char *user,
                        const unsigned char *data, short dlen)
{
    int attempt;

    for (attempt = 0; attempt < 2; attempt++) {
        OSErr err;
        memset(&atp_bds, 0, sizeof(atp_bds));
        atp_bds.buffSize = (short)sizeof(rx_buf);
        atp_bds.buffPtr = rx_buf;

        memset(&atp_pb, 0, sizeof(atp_pb));
        atp_pb.ioRefNum = atp_ref;
        atp_pb.csCode = nSendRequest;
        atp_pb.atpSocket = atp_socket;
        atp_pb.atpFlags = atpXOvalue;
        atp_pb.addrBlock[0] = 0;               /* network number */
        atp_pb.addrBlock[1] = 0;
        atp_pb.addrBlock[2] = server_node;     /* node ID */
        atp_pb.addrBlock[3] = server_socket;   /* socket */
        atp_pb.reqLength = dlen;
        atp_pb.reqPointer = (void *)data;
        atp_pb.bdsPointer = &atp_bds;
        atp_pb.numOfBuffs = 1;
        atp_pb.timeOutVal = 2;                 /* seconds */
        atp_pb.retryCount = 3;
        atp_pb.userData = ((long)user[0] << 24) | ((long)user[1] << 16)
                          | ((long)user[2] << 8) | (long)user[3];

        err = PBControl(atp_ref, &atp_pb, false);
        if (err != noErr) {
            printf("PBControl error %d (ioResult %d)\n", (short)err,
                   (short)atp_pb.ioResult);
            log_line("PBControl error %d (ioResult %d)", (short)err,
                     (short)atp_pb.ioResult);
            return -1;
        }
        if (atp_pb.ioResult == noErr) {
            rsp_len = atp_bds.dataSize;
            rsp_user[0] = (unsigned char)(atp_bds.userBytes >> 24);
            rsp_user[1] = (unsigned char)(atp_bds.userBytes >> 16);
            rsp_user[2] = (unsigned char)(atp_bds.userBytes >> 8);
            rsp_user[3] = (unsigned char)atp_bds.userBytes;
            rx_len = rsp_len;
            log_line("ATP ok: %d data bytes, user %02x%02x%02x%02x",
                     rsp_len, rsp_user[0], rsp_user[1], rsp_user[2],
                     rsp_user[3]);
            return 1;
        }
        printf("ATP request failed (%d), retrying\n",
               (short)atp_pb.ioResult);
        log_line("ATP request failed (%d), retrying",
                 (short)atp_pb.ioResult);
    }
    return 0;
}

/* Whether the last reply's 4-byte command result is noErr */
static int result_ok(void)
{
    return rsp_user[0] == 0 && rsp_user[1] == 0
           && rsp_user[2] == 0 && rsp_user[3] == 0;
}

/* Send one AFP command over the open ASP session and wait for the
 * reply. The reply's 4-byte command result ends up in rsp_user, the
 * reply data in rx_buf/rx_len.
 * Returns 1 on reply, 0 on timeout, -1 on PBControl error. */
static int afp_exchange(short atp_ref, unsigned char session_id,
                        const unsigned char *cmd, short clen)
{
    unsigned char user[4];

    user[0] = 2;              /* Command */
    user[1] = session_id;
    asp_seq++;
    user[2] = (asp_seq >> 8) & 0xFF;
    user[3] = asp_seq & 0xFF;
    return asp_exchange(atp_ref, user, cmd, clen);
}

/* Like asp_exchange, but offering nbufs response buffers. A reply that
 * needs several ATP response packets is delivered into mp_bufs (the
 * .ATP driver reports the per-packet byte counts in mp_fill and the
 * packet count in mp_resps; no compaction is done, so the reply must
 * be compared in place). The reply's 4 user bytes land in rsp_user.
 * The guest's own .ATP driver validates the multi-packet encoding
 * (sequence numbers, EOM), so a malformed split shows up as a retry
 * here.
 * Returns 1 on reply, 0 on timeout, -1 on PBControl error. */
static int asp_exchange_multi(short atp_ref, const unsigned char *user,
                              const unsigned char *data, short dlen,
                              short nbufs)
{
    int attempt;
    short i;

    for (attempt = 0; attempt < 2; attempt++) {
        OSErr err;
        memset(mp_bds, 0, sizeof(mp_bds));
        for (i = 0; i < nbufs; i++) {
            mp_bds[i].buffSize = MP_BUF_SIZE;
            mp_bds[i].buffPtr = mp_bufs[i];
        }

        memset(&atp_pb, 0, sizeof(atp_pb));
        atp_pb.ioRefNum = atp_ref;
        atp_pb.csCode = nSendRequest;
        atp_pb.atpSocket = atp_socket;
        atp_pb.atpFlags = atpXOvalue;
        atp_pb.addrBlock[0] = 0;               /* network number */
        atp_pb.addrBlock[1] = 0;
        atp_pb.addrBlock[2] = server_node;     /* node ID */
        atp_pb.addrBlock[3] = server_socket;   /* socket */
        atp_pb.reqLength = dlen;
        atp_pb.reqPointer = (void *)data;
        atp_pb.bdsPointer = mp_bds;
        atp_pb.numOfBuffs = (unsigned char)nbufs;
        atp_pb.timeOutVal = 2;                 /* seconds */
        atp_pb.retryCount = 3;
        atp_pb.userData = ((long)user[0] << 24) | ((long)user[1] << 16)
                          | ((long)user[2] << 8) | (long)user[3];

        err = PBControl(atp_ref, &atp_pb, false);
        if (err != noErr) {
            printf("PBControl error %d (ioResult %d)\n", (short)err,
                   (short)atp_pb.ioResult);
            log_line("PBControl error %d (ioResult %d)", (short)err,
                     (short)atp_pb.ioResult);
            return -1;
        }
        if (atp_pb.ioResult == noErr) {
            memset(mp_fill, 0, sizeof(mp_fill));
            for (i = 0; i < (short)atp_pb.numOfResps && i < nbufs; i++)
                mp_fill[i] = mp_bds[i].dataSize;
            mp_resps = atp_pb.numOfResps;
            rsp_user[0] = (unsigned char)(mp_bds[0].userBytes >> 24);
            rsp_user[1] = (unsigned char)(mp_bds[0].userBytes >> 16);
            rsp_user[2] = (unsigned char)(mp_bds[0].userBytes >> 8);
            rsp_user[3] = (unsigned char)mp_bds[0].userBytes;
            log_line("ATP multi ok: %d packets, fills %d/%d/%d, "
                     "user %02x%02x%02x%02x", mp_resps, mp_fill[0],
                     mp_fill[1], mp_fill[2], rsp_user[0], rsp_user[1],
                     rsp_user[2], rsp_user[3]);
            return 1;
        }
        printf("ATP request failed (%d), retrying\n",
               (short)atp_pb.ioResult);
        log_line("ATP request failed (%d), retrying",
                 (short)atp_pb.ioResult);
    }
    return 0;
}

/* Like afp_exchange, but for a reply that needs several ATP response
 * packets (the assembled data lands in big_rx/big_len) */
static int afp_exchange_multi(short atp_ref, unsigned char session_id,
                              const unsigned char *cmd, short clen,
                              short nbufs)
{
    unsigned char user[4];

    user[0] = 2;              /* Command */
    user[1] = session_id;
    asp_seq++;
    user[2] = (asp_seq >> 8) & 0xFF;
    user[3] = asp_seq & 0xFF;
    return asp_exchange_multi(atp_ref, user, cmd, clen, nbufs);
}

int main(void)
{
    unsigned char send_data[4 + 64];
    char fail_msg[200] = "";
    char line[200];
    short mpp_ref;
    OSErr err;
    bool ok = true;
    int i;

    printf("ATalkTest: AppleTalk DDP loopback test\n");

    err = OpenDriver((ConstStringPtr)"\p.MPP", &mpp_ref);
    if (err != noErr) {
        printf("OpenDriver(.MPP) failed: %d\n", (short)err);
        snprintf(fail_msg, sizeof(fail_msg), "FAIL: OpenDriver(.MPP) failed (%d)\n", (short)err);
        ok = false;
        goto done;
    }

    /* Open the .ATP driver and an ATP socket for the ASP transactions */
    err = OpenDriver((ConstStringPtr)"\p.ATP", &atp_ref);
    if (err != noErr) {
        printf("OpenDriver(.ATP) failed: %d\n", (short)err);
        snprintf(fail_msg, sizeof(fail_msg), "FAIL: OpenDriver(.ATP) failed (%d)\n", (short)err);
        ok = false;
        goto done;
    }
    memset(&atp_pb, 0, sizeof(atp_pb));
    atp_pb.ioRefNum = atp_ref;
    atp_pb.csCode = openATPSkt;
    atp_pb.atpSocket = 0;      /* dynamically assigned */
    err = PBControl(atp_ref, &atp_pb, false);
    if (err != noErr || atp_pb.ioResult != noErr) {
        printf("POpenATPSkt failed: %d, ioResult %d\n", (short)err,
               (short)atp_pb.ioResult);
        snprintf(fail_msg, sizeof(fail_msg),
                 "FAIL: POpenATPSkt failed (%d, %d)\n", (short)err,
                 (short)atp_pb.ioResult);
        ok = false;
        goto done;
    }
    atp_socket = atp_pb.atpSocket;
    printf("ATP socket %d\n", atp_socket);
    log_line("ATP socket %d", atp_socket);

    progress("stage: .MPP open\n");

    atalk_listener_init(rx_buf, &rx_flag, &rx_len, &rx_socket);

    /* Open the test socket */
    memset(&pb, 0, sizeof(pb));
    pb.ioRefNum = mpp_ref;
    pb.csCode = openSkt;
    pb.socket = TEST_SOCKET;
    pb.listener = (void *)atalk_listener;
    err = PBControl(mpp_ref, &pb, false);
    if (err != noErr || pb.ioResult != noErr) {
        printf("POpenSkt failed: PBControl %d, ioResult %d. Is AppleTalk active on the printer port?\n",
               (short)err, (short)pb.ioResult);
        snprintf(fail_msg, sizeof(fail_msg),
                 "FAIL: POpenSkt failed, PBControl %d, ioResult %d\n",
                 (short)err, (short)pb.ioResult);
        ok = false;
        goto done;
    }
    printf("Socket %d open\n", pb.socket);
    progress("stage: socket open\n");

    /* Set up the write-data structure once */
    memset(wds_hdr, 0, sizeof(wds_hdr));
    memset(&wds, 0, sizeof(wds));
    wds.header = wds_hdr + 1;
    wds.data = send_data;

    /* Send test packets and check the echoes */
    for (i = 0; i < TEST_PACKETS && ok; i++) {
        int len;

        memcpy(send_data, TEST_MAGIC, 4);
        len = 4 + sprintf((char *)send_data + 4, "ATalkTest packet %d", i);

        /* Destination: network 0, node 0xFF (broadcast), socket TEST_SOCKET */
        wds_hdr[8] = 0;
        wds_hdr[9] = 0;
        wds_hdr[12] = 0xFF;
        wds_hdr[14] = TEST_SOCKET;
        wds_hdr[16] = TEST_TYPE;
        wds.data_len = (short)len;

        memset(&pb, 0, sizeof(pb));
        pb.ioRefNum = mpp_ref;
        pb.csCode = writeDDP;
        pb.socket = TEST_SOCKET;
        pb.checksumFlag = 0;
        pb.listener = (void *)&wds;
        rx_flag = 0;
        err = PBControl(mpp_ref, &pb, false);
        if (err != noErr) {
            printf("PWriteDDP failed: PBControl %d\n", (short)err);
            snprintf(fail_msg, sizeof(fail_msg),
                     "FAIL: PWriteDDP failed, PBControl %d\n", (short)err);
            ok = false;
            break;
        }

        snprintf(line, sizeof(line), "stage: wrote packet %d (ticks %lu)\n",
                 i, (unsigned long)TickCount());
        progress(line);

        /* Wait for the echo (10 seconds) */
        {
            long deadline = TickCount() + 60 * 10;
            while (!rx_flag && TickCount() < deadline)
                ;
            if (!rx_flag) {
                printf("No echo for packet %d\n", i);
                snprintf(fail_msg, sizeof(fail_msg),
                         "FAIL: no echo for packet %d after 10s\n", i);
                ok = false;
                break;
            }
        }

        if (rx_flag)
            progress("stage: flag set after wait\n");

        if (rx_len != len || memcmp(rx_buf, send_data, (size_t)len) != 0) {
            printf("Bad echo for packet %d (len %d, expected %d)\n", i, rx_len, len);
            snprintf(fail_msg, sizeof(fail_msg),
                     "FAIL: bad echo for packet %d (len %d, expected %d)\n",
                     i, rx_len, len);
            ok = false;
            break;
        }
        printf("Packet %d echoed (%d bytes) on socket %d\n", i, len, rx_socket);
    }

    /* NBP: broadcast a lookup for the test entity (Snow:AFPServer@*) and
     * check the LookUp-Reply */
    if (ok) {
        unsigned char brd[2 + 5 + 1 + 1 + 1 + 1 + 1 + 1];
        int n = 0;
        const unsigned char *names;

        brd[n++] = (1 << 4) | 1;      /* BrdRq, one tuple */
        brd[n++] = 0x42;              /* NBP ID */
        brd[n++] = 0; brd[n++] = 0;   /* requester network 0 */
        brd[n++] = 0;                 /* requester node (self) */
        brd[n++] = 0;                 /* requester socket (self) */
        brd[n++] = 0;                 /* enumerator */
        brd[n++] = 1; brd[n++] = '='; /* object wildcard */
        brd[n++] = 1; brd[n++] = '='; /* type wildcard */
        brd[n++] = 1; brd[n++] = '*'; /* zone wildcard */

        /* Destination: network 0, node 0xFF (broadcast), NBP socket 2,
         * DDP type 2 */
        wds_hdr[8] = 0;
        wds_hdr[9] = 0;
        wds_hdr[12] = 0xFF;
        wds_hdr[14] = 2;
        wds_hdr[16] = 2;
        wds.data = brd;
        wds.data_len = (short)n;

        memset(&pb, 0, sizeof(pb));
        pb.ioRefNum = mpp_ref;
        pb.csCode = writeDDP;
        pb.socket = TEST_SOCKET;
        pb.checksumFlag = 0;
        pb.listener = (void *)&wds;
        rx_flag = 0;
        err = PBControl(mpp_ref, &pb, false);
        if (err != noErr) {
            printf("NBP BrdRq failed: PBControl %d\n", (short)err);
            snprintf(fail_msg, sizeof(fail_msg),
                     "FAIL: NBP BrdRq failed, PBControl %d\n", (short)err);
            ok = false;
        } else {
            long deadline = TickCount() + 60 * 10;
            while (!rx_flag && TickCount() < deadline)
                ;
            if (!rx_flag) {
                printf("No NBP reply\n");
                snprintf(fail_msg, sizeof(fail_msg),
                         "FAIL: no NBP reply after 10s\n");
                ok = false;
            } else if (rx_len < 7 || rx_buf[0] != ((3 << 4) | 1)
                       || rx_buf[1] != 0x42) {
                printf("Unexpected NBP reply (len %d)\n", rx_len);
                snprintf(fail_msg, sizeof(fail_msg),
                         "FAIL: unexpected NBP reply (len %d)\n", rx_len);
                ok = false;
            } else {
                /* Tuple: network(2) node(1) socket(1) enumerator(1)
                 * followed by the object, type and zone strings */
                names = rx_buf + 2 + 5;
                if (names[0] != 4 || memcmp(names + 1, "Snow", 4) != 0
                    || names[5] != 9
                    || memcmp(names + 6, "AFPServer", 9) != 0) {
                    printf("Unexpected NBP entity\n");
                    snprintf(fail_msg, sizeof(fail_msg),
                             "FAIL: unexpected NBP entity\n");
                    ok = false;
                } else {
                    printf("NBP: Snow:AFPServer on node %d socket %d\n",
                           rx_buf[4], rx_buf[5]);
                    progress("stage: NBP lookup OK\n");
                    server_node = rx_buf[4];
                    server_socket = rx_buf[5];

                    /* AEP echo: the node must answer an Echo Request
                     * (function 1) with an Echo Reply (function 2)
                     * carrying the same data */
                    {
                        static unsigned char aep_data[6] = {
                            1, 'A', 'E', 'P', 'o', 'k'
                        };
                        long deadline;

                        wds.data = aep_data;
                        wds.data_len = 6;
                        wds_hdr[8] = 0;
                        wds_hdr[9] = 0;
                        wds_hdr[12] = server_node;
                        wds_hdr[14] = 4;      /* AEP socket */
                        wds_hdr[16] = 4;      /* AEP DDP type */
                        memset(&pb, 0, sizeof(pb));
                        pb.ioRefNum = mpp_ref;
                        pb.csCode = writeDDP;
                        pb.socket = TEST_SOCKET;
                        pb.checksumFlag = 0;
                        pb.listener = (void *)&wds;
                        rx_flag = 0;
                        if (PBControl(mpp_ref, &pb, false) != noErr) {
                            printf("AEP writeDDP failed\n");
                            snprintf(fail_msg, sizeof(fail_msg),
                                     "FAIL: AEP writeDDP failed\n");
                            ok = false;
                        } else {
                            deadline = TickCount() + 60 * 10;
                            while (!rx_flag && TickCount() < deadline)
                                ;
                            if (!rx_flag || rx_len != 6
                                || rx_buf[0] != 2
                                || memcmp(rx_buf + 1, aep_data + 1,
                                          5) != 0) {
                                printf("AEP echo failed (len %d)\n",
                                       rx_len);
                                snprintf(fail_msg, sizeof(fail_msg),
                                         "FAIL: AEP echo failed\n");
                                ok = false;
                            } else {
                                printf("AEP echo OK\n");
                                progress("stage: AEP echo OK\n");
                            }
                        }
                    }

                    /* ASP: open a session to the node, send a command,
                     * tickle and close the session (unicast transactions
                     * over the node's ATP socket, exercising the RTS/CTS
                     * path). If no reply arrives (the node can drop
                     * responses for testing), the same request is sent
                     * again and must be answered from the node's
                     * retransmit cache. */
                    {
                        static const unsigned char open_sess[4] = {
                            4, 0x64, 0, 1
                        };  /* [OpenSess][WSS][version] */
                        unsigned char user[4];
                        int r;
                        unsigned char session_id;

                        r = asp_exchange(atp_ref, open_sess, NULL, 0);
                        if (r != 1 || rsp_user[2] != 0 || rsp_user[3] != 0
                            || rsp_user[1] == 0) {
                            printf("ASP OpenSess failed (%d)\n", r);
                            snprintf(fail_msg, sizeof(fail_msg),
                                     "FAIL: ASP OpenSess failed (%d)\n", r);
                            ok = false;
                        } else {
                            session_id = rsp_user[1];
                            printf("ASP session %d opened (SSS %d)\n",
                                   session_id, rsp_user[0]);

                            /* AFP: query the server information, log in, open the
                             * volume, enumerate it and read a file back. If no reply
                             * arrives (the node can drop responses for testing), the
                             * same request is sent again and must be answered from
                             * the node's retransmit cache. */
                            {
                                static const unsigned char get_status[4] = { 3, 0, 0 };
                                static const char expect[] =
                                    "Hello from the Snow AFP server!";
                                unsigned char cmd[64];
                                int n;
                                unsigned short fork_ref;
                                unsigned char mname[32];
                                int mlen, found;
                            
                                /* FPGetSrvrInfo, via the ASP GetStatus */
                                r = asp_exchange(atp_ref, get_status, NULL, 0);
                                if (r != 1 || rx_len < 15
                                    || rx_buf[10] != 4
                                    || memcmp(rx_buf + 11, "Snow", 4) != 0) {
                                    printf("Bad server info (%d)\n", r);
                                    snprintf(fail_msg, sizeof(fail_msg),
                                             "FAIL: bad server info (%d)\n",
                                             r);
                                    ok = false;
                                } else {
                                    printf("Server info OK\n");
                            
                                    /* FPLogin: guest authentication */
                                    n = 1;
                                    cmd[0] = 18;
                                    cmd[n++] = 6;
                                    memcpy(cmd + n, "AFP2.0", 6); n += 6;
                                    cmd[n++] = 15;
                                    memcpy(cmd + n, "No User Authent", 15); n += 15;
                                    r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                    if (r != 1 || !result_ok()) {
                                        printf("AFP login failed (%d)\n", r);
                                        snprintf(fail_msg, sizeof(fail_msg),
                                                 "FAIL: AFP login failed (%d)\n", r);
                                        ok = false;
                                    } else {
                                        printf("AFP logged in\n");
                            
                                        /* FPGetSrvrParms: the volume list */
                                        cmd[0] = 16; cmd[1] = 0;
                                        r = afp_exchange(atp_ref, session_id, cmd, 2);
                                        if (r != 1 || !result_ok() || rx_buf[4] != 1
                                            || rx_buf[6] != 4
                                            || memcmp(rx_buf + 7, "Snow", 4) != 0) {
                                            printf("Bad server parms (%d)\n", r);
                                            snprintf(fail_msg, sizeof(fail_msg),
                                                     "FAIL: bad server parms (%d)\n", r);
                                            ok = false;
                                        } else {
                                            printf("Volume 'Snow' listed\n");
                            
                                            /* FPOpenVol */
                                            n = 2;
                                            cmd[0] = 24; cmd[1] = 0;
                                            cmd[n++] = 0x01; cmd[n++] = 0x20;
                                            cmd[n++] = 4;
                                            memcpy(cmd + n, "Snow", 4); n += 4;
                                            r = afp_exchange(atp_ref, session_id, cmd,
                                                             (short)n);
                                            if (r != 1 || !result_ok()
                                                || rx_buf[2] != 0 || rx_buf[3] != 1) {
                                                printf("AFP OpenVol failed (%d)\n", r);
                                                snprintf(fail_msg, sizeof(fail_msg),
                                                         "FAIL: AFP OpenVol failed (%d)\n",
                                                         r);
                                                ok = false;
                                            } else {
                                                printf("Volume opened\n");
                            
                                                /* FPEnumerate: list the volume root */
                                                n = 2;
                                                cmd[0] = 9; cmd[1] = 0;
                                                cmd[n++] = 0; cmd[n++] = 1; /* volume */
                                                cmd[n++] = 0; cmd[n++] = 0;
                                                cmd[n++] = 0; cmd[n++] = 2; /* root dir */
                                                cmd[n++] = 0x03; cmd[n++] = 0x40;
                                                cmd[n++] = 0x03; cmd[n++] = 0x40; /* dir bits */
                                                cmd[n++] = 0; cmd[n++] = 20; /* count */
                                                cmd[n++] = 0; cmd[n++] = 1; /* start */
                                                cmd[n++] = 0x02; cmd[n++] = 0x00;
                                                cmd[n++] = 2; /* long names */
                                                cmd[n++] = 0; /* the root */
                                                r = afp_exchange(atp_ref, session_id, cmd,
                                                                 (short)n);
                                                if (r != 1 || !result_ok()
                                                    || (rx_buf[4] == 0
                                                        && rx_buf[5] == 0)) {
                                                    printf("AFP Enumerate failed (%d)\n",
                                                           r);
                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                             "FAIL: AFP Enumerate failed "
                                                             "(%d)\n", r);
                                                    ok = false;
                                                } else {
                                                    printf("Volume has %d entries\n",
                                                           (rx_buf[4] << 8)
                                                           | rx_buf[5]);

                                                    /* Find the "sub" directory's
                                                     * Directory ID among the records
                                                     * (bit 8 follows the name offset),
                                                     * enumerate it BY ID, and check it
                                                     * lists its own offspring */
                                                    {
                                                        int cnt = (rx_buf[4] << 8) | rx_buf[5];
                                                        int pos = 6;
                                                        int i;
                                                        unsigned long sub_id = 0;
                                                        unsigned char sub_cmd[64];
                                                        int m;

                                                        for (i = 0; i < cnt; i++) {
                                                            int rec_len = rx_buf[pos];
                                                            if (rec_len < 3) {
                                                                pos += 2;
                                                                continue;
                                                            }
                                                            if ((rx_buf[pos + 1] & 0x80) != 0) {
                                                                /* a directory: name offset,
                                                                 * then the Directory ID */
                                                                sub_id = ((unsigned long)rx_buf[pos + 4] << 24)
                                                                       | ((unsigned long)rx_buf[pos + 5] << 16)
                                                                       | ((unsigned long)rx_buf[pos + 6] << 8)
                                                                       | (unsigned long)rx_buf[pos + 7];
                                                                break;
                                                            }
                                                            pos += rec_len;
                                                        }
                                                        if (sub_id == 0) {
                                                            printf("No subdirectory record\n");
                                                            snprintf(fail_msg, sizeof(fail_msg),
                                                                     "FAIL: no subdirectory "
                                                                     "record\n");
                                                            ok = false;
                                                        } else {
                                                            m = 2;
                                                            sub_cmd[0] = 9; sub_cmd[1] = 0;
                                                            sub_cmd[m++] = 0; sub_cmd[m++] = 1;
                                                            sub_cmd[m++] = (unsigned char)(sub_id >> 24);
                                                            sub_cmd[m++] = (unsigned char)(sub_id >> 16);
                                                            sub_cmd[m++] = (unsigned char)(sub_id >> 8);
                                                            sub_cmd[m++] = (unsigned char)sub_id;
                                                            sub_cmd[m++] = 0x03; sub_cmd[m++] = 0x40;
                                                            sub_cmd[m++] = 0x03; sub_cmd[m++] = 0x40;
                                                            sub_cmd[m++] = 0; sub_cmd[m++] = 20;
                                                            sub_cmd[m++] = 0; sub_cmd[m++] = 1;
                                                            sub_cmd[m++] = 0x02; sub_cmd[m++] = 0x00;
                                                            sub_cmd[m++] = 2;
                                                            sub_cmd[m++] = 0;
                                                            r = afp_exchange(atp_ref, session_id,
                                                                             sub_cmd, (short)m);
                                                            if (r != 1 || !result_ok()
                                                                || (rx_buf[4] == 0 && rx_buf[5] == 0)) {
                                                                printf("AFP EnumerateSub failed (%d)\n", r);
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: AFP EnumerateSub "
                                                                         "failed (%d)\n", r);
                                                                ok = false;
                                                            } else {
                                                                /* The first record is inner.txt */
                                                                int rl = rx_buf[6];
                                                                int off = (rx_buf[8] << 8) | rx_buf[9];
                                                                if ((rx_buf[7] & 0x80) != 0
                                                                    || rx_buf[6 + 2 + off] != 9
                                                                    || memcmp(rx_buf + 6 + 2 + off + 1,
                                                                              "inner.txt", 9) != 0) {
                                                                    printf("Subdirectory lists "
                                                                           "the wrong entries\n");
                                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                                             "FAIL: subdirectory lists "
                                                                             "the wrong entries\n");
                                                                    ok = false;
                                                                } else {
                                                                    printf("Subdirectory opened "
                                                                           "by ID OK (%d bytes)\n", rl);
                                                                }
                                                            }
                                                        }
                                                    }

                                                    /* FPOpenFork on the data fork */
                                                    n = 2;
                                                    cmd[0] = 26; cmd[1] = 0; /* data fork */
                                                    cmd[n++] = 0; cmd[n++] = 1; /* vol */
                                                    cmd[n++] = 0; cmd[n++] = 0;
                                                    cmd[n++] = 0; cmd[n++] = 2; /* dir */
                                                    cmd[n++] = 0; cmd[n++] = 0; /* bits */
                                                    cmd[n++] = 0; cmd[n++] = 1; /* read */
                                                    cmd[n++] = 2; /* long name */
                                                    cmd[n++] = 9;
                                                    memcpy(cmd + n, "hello.txt", 9);
                                                    n += 9;
                                                    r = afp_exchange(atp_ref, session_id,
                                                                     cmd, (short)n);
                                                    if (r != 1 || !result_ok()) {
                                                        printf("AFP OpenFork failed (%d)\n",
                                                               r);
                                                        snprintf(fail_msg, sizeof(fail_msg),
                                                                 "FAIL: AFP OpenFork failed "
                                                                 "(%d)\n", r);
                                                        ok = false;
                                                    } else {
                                                        fork_ref = (unsigned short)
                                                            ((rx_buf[2] << 8) | rx_buf[3]);
                                                        printf("Fork %d opened\n",
                                                               fork_ref);
                            
                                                        /* FPRead */
                                                        n = 2;
                                                        cmd[0] = 27; cmd[1] = 0;
                                                        cmd[n++] = (fork_ref >> 8) & 0xFF;
                                                        cmd[n++] = fork_ref & 0xFF;
                                                        cmd[n++] = 0; cmd[n++] = 0;
                                                        cmd[n++] = 0; cmd[n++] = 0;
                                                        cmd[n++] = 0; cmd[n++] = 0;
                                                        cmd[n++] = 0; cmd[n++] = 64;
                                                        cmd[n++] = 0; cmd[n++] = 0;
                                                        r = afp_exchange(atp_ref, session_id,
                                                                         cmd, (short)n);
                                                        if (r != 1 || !result_ok()
                                                            || rx_len !=
                                                               (short)sizeof(expect) - 1
                                                            || memcmp(rx_buf, expect,
                                                                     sizeof(expect) - 1)
                                                               != 0) {
                                                            printf("AFP Read failed (%d)\n",
                                                                   r);
                                                            snprintf(fail_msg,
                                                                     sizeof(fail_msg),
                                                                     "FAIL: AFP Read failed "
                                                                     "(%d)\n", r);
                                                        } else {
                                                            printf("File content OK\n");

                                                            /* Multi-packet ATP: read 'big.txt'
                                                             * (1500 bytes) back in one FPRead. The
                                                             * reply needs three response packets;
                                                             * the guest's own .ATP driver validates
                                                             * the split (sequence numbers, EOM) and
                                                             * reassembles the buffers. */
                                                            n = 2;
                                                            cmd[0] = 26; cmd[1] = 0; /* data fork */
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 2;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 2;
                                                            cmd[n++] = 7;
                                                            memcpy(cmd + n, "big.txt", 7); n += 7;
                                                            r = afp_exchange(atp_ref, session_id,
                                                                             cmd, (short)n);
                                                            if (r != 1 || !result_ok()) {
                                                                printf("AFP OpenBig failed (%d)\n", r);
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: AFP OpenBig failed (%d)\n",
                                                                         r);
                                                                ok = false;
                                                            } else {
                                                                int j;

                                                                fork_ref = (unsigned short)
                                                                    ((rx_buf[2] << 8) | rx_buf[3]);
                                                                n = 2;
                                                                cmd[0] = 27; cmd[1] = 0;
                                                                cmd[n++] = (fork_ref >> 8) & 0xFF;
                                                                cmd[n++] = fork_ref & 0xFF;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0x05; cmd[n++] = 0xDC; /* 1500 */
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                r = afp_exchange_multi(atp_ref, session_id,
                                                                                       cmd, (short)n,
                                                                                       MP_NBUF);
                                                                for (j = 0;
                                                                     j < (int)sizeof(big_expect);
                                                                     j++)
                                                                    big_expect[j] = (unsigned char)
                                                                        ('0' + j % 10);
                                                                /* The 1500-byte reply travels as
                                                                 * three packets: 578 + 578 + 344
                                                                 * data bytes, compared in place in
                                                                 * the contiguous 578-byte buffers */
                                                                if (r != 1 || !result_ok()
                                                                    || mp_resps != 3
                                                                    || mp_fill[0] != 578
                                                                    || mp_fill[1] != 578
                                                                    || mp_fill[2] != 344
                                                                    || memcmp(mp_bufs[0], big_expect,
                                                                              578) != 0
                                                                    || memcmp(mp_bufs[1],
                                                                              big_expect + 578,
                                                                              578) != 0
                                                                    || memcmp(mp_bufs[2],
                                                                              big_expect + 1156,
                                                                              344) != 0) {
                                                                    printf("AFP ReadBig failed "
                                                                           "(%d, %d packets, fills "
                                                                           "%d/%d/%d)\n", r,
                                                                           mp_resps, mp_fill[0],
                                                                           mp_fill[1], mp_fill[2]);
                                                                    snprintf(fail_msg,
                                                                             sizeof(fail_msg),
                                                                             "FAIL: AFP ReadBig "
                                                                             "failed (%d)\n", r);
                                                                    ok = false;
                                                                } else {
                                                                    printf("Multi-packet read OK "
                                                                           "(%d packets)\n",
                                                                           mp_resps);
                                                                    cmd[0] = 4; cmd[1] = 0;
                                                                    cmd[2] = (fork_ref >> 8) & 0xFF;
                                                                    cmd[3] = fork_ref & 0xFF;
                                                                    r = afp_exchange(atp_ref,
                                                                                     session_id,
                                                                                     cmd, 4);
                                                                    if (r != 1 || !result_ok()) {
                                                                        printf("AFP CloseBig failed "
                                                                               "(%d)\n", r);
                                                                        snprintf(fail_msg,
                                                                                 sizeof(fail_msg),
                                                                                 "FAIL: AFP CloseBig "
                                                                                 "failed (%d)\n", r);
                                                                        ok = false;
                                                                    }
                                                                }
                                                            }

                                                            /* The resource fork: FPOpenFork with the
                                                             * Rsrc/Data flag set, then FPRead, on the
                                                             * '._hello.txt' AppleDouble sidecar */
                                                            n = 2;
                                                            cmd[0] = 26; cmd[1] = 0x80; /* rsrc fork */
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 2;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 2;
                                                            cmd[n++] = 9;
                                                            memcpy(cmd + n, "hello.txt", 9); n += 9;
                                                            r = afp_exchange(atp_ref, session_id, cmd,
                                                                             (short)n);
                                                            if (r != 1 || !result_ok()) {
                                                                printf("AFP OpenRsrcFork failed (%d)\n", r);
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: AFP OpenRsrcFork failed (%d)\n",
                                                                         r);
                                                                ok = false;
                                                            } else {
                                                                printf("Resource fork opened\n");
                                                                fork_ref = (unsigned short)
                                                                    ((rx_buf[2] << 8) | rx_buf[3]);
                                                                n = 2;
                                                                cmd[0] = 27; cmd[1] = 0;
                                                                cmd[n++] = (fork_ref >> 8) & 0xFF;
                                                                cmd[n++] = fork_ref & 0xFF;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 64;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                r = afp_exchange(atp_ref, session_id, cmd,
                                                                                 (short)n);
                                                                if (r != 1 || !result_ok() || rx_len != 9
                                                                    || memcmp(rx_buf, "RESOURCE!", 9) != 0) {
                                                                    printf("AFP ReadRsrc failed (%d)\n", r);
                                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                                             "FAIL: AFP ReadRsrc failed (%d)\n", r);
                                                                    ok = false;
                                                                } else {
                                                                    printf("Resource fork content OK\n");
                                                                    cmd[0] = 4; cmd[1] = 0;
                                                                    cmd[2] = (fork_ref >> 8) & 0xFF;
                                                                    cmd[3] = fork_ref & 0xFF;
                                                                    r = afp_exchange(atp_ref, session_id, cmd, 4);
                                                                    if (r != 1 || !result_ok()) {
                                                                        printf("AFP CloseRsrc failed (%d)\n", r);
                                                                        snprintf(fail_msg, sizeof(fail_msg),
                                                                                 "FAIL: AFP CloseRsrc failed (%d)\n", r);
                                                                        ok = false;
                                                                    }
                                                                }
                                                            }

                                                            /* The ':' <-> '/' bijection: host a:b.txt is a/b.txt on the wire */
                                                            n = 2;
                                                            cmd[0] = 26; cmd[1] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 2;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 2;
                                                            cmd[n++] = 7;
                                                            memcpy(cmd + n, "a/b.txt", 7); n += 7;
                                                            r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                                            if (r != 1 || !result_ok()) {
                                                                printf("AFP OpenColon failed (%d)\n", r);
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: AFP OpenColon failed (%d)\n", r);
                                                                ok = false;
                                                            } else {
                                                                fork_ref = (unsigned short)
                                                                    ((rx_buf[2] << 8) | rx_buf[3]);
                                                                n = 2;
                                                                cmd[0] = 27; cmd[1] = 0;
                                                                cmd[n++] = (fork_ref >> 8) & 0xFF;
                                                                cmd[n++] = fork_ref & 0xFF;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 64;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                                                if (r != 1 || !result_ok() || rx_len != 5
                                                                    || memcmp(rx_buf, "slash", 5) != 0) {
                                                                    printf("AFP ReadColon failed (%d)\n", r);
                                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                                             "FAIL: AFP ReadColon failed (%d)\n", r);
                                                                    ok = false;
                                                                } else {
                                                                    printf("Colon bijection OK\n");
                                                                    cmd[0] = 4; cmd[1] = 0;
                                                                    cmd[2] = (fork_ref >> 8) & 0xFF;
                                                                    cmd[3] = fork_ref & 0xFF;
                                                                    r = afp_exchange(atp_ref, session_id, cmd, 4);
                                                                    if (r != 1 || !result_ok()) {
                                                                        printf("AFP CloseColon failed (%d)\n", r);
                                                                        snprintf(fail_msg, sizeof(fail_msg),
                                                                                 "FAIL: AFP CloseColon failed (%d)\n", r);
                                                                        ok = false;
                                                                    }
                                                                }
                                                            }

                                                            /* A mangled name: enumerate again, find the entry whose
                                                             * name contains "#", open it and read it back */
                                                            n = 2;
                                                            cmd[0] = 9; cmd[1] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 2;
                                                            cmd[n++] = 0x03; cmd[n++] = 0x40;
                                                            cmd[n++] = 0; cmd[n++] = 0;
                                                            cmd[n++] = 0; cmd[n++] = 20;
                                                            cmd[n++] = 0; cmd[n++] = 1;
                                                            cmd[n++] = 0x02; cmd[n++] = 0x00;
                                                            cmd[n++] = 2;
                                                            cmd[n++] = 0;
                                                            r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                                            found = 0;
                                                            if (r != 1 || !result_ok()) {
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: second AFP Enumerate failed (%d)\n", r);
                                                                ok = false;
                                                            } else {
                                                                {
                                                                    int cnt = (rx_buf[4] << 8) | rx_buf[5];
                                                                    int pos = 6;
                                                                    int i;
                                                                    for (i = 0; i < cnt && !found; i++) {
                                                                        int rec_len = rx_buf[pos];
                                                                        if (rx_buf[pos + 1] == 0) { /* a file */
                                                                            int off = (rx_buf[pos + 2] << 8)
                                                                                | rx_buf[pos + 3];
                                                                            int name_pos = pos + 2 + off;
                                                                            int name_len = rx_buf[name_pos];
                                                                            int j;
                                                                            for (j = 1; j <= name_len; j++) {
                                                                                if (rx_buf[name_pos + j] == (unsigned char)'#') {
                                                                                    found = 1;
                                                                                    break;
                                                                                }
                                                                            }
                                                                            if (found) {
                                                                                mlen = name_len;
                                                                                memcpy(mname, rx_buf + name_pos,
                                                                                       (size_t)name_len + 1);
                                                                            }
                                                                        }
                                                                        pos += rec_len;
                                                                    }
                                                                }
                                                            }
                                                            if (!found) {
                                                                printf("Mangled entry not found\n");
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: mangled entry not found\n");
                                                                ok = false;
                                                            } else {
                                                                printf("Mangled name found (%.*s)\n", mlen, mname + 1);
                                                                n = 2;
                                                                cmd[0] = 26; cmd[1] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 1;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 2;
                                                                cmd[n++] = 0; cmd[n++] = 0;
                                                                cmd[n++] = 0; cmd[n++] = 1;
                                                                cmd[n++] = 2;
                                                                cmd[n++] = (unsigned char)mlen;
                                                                memcpy(cmd + n, mname + 1, (size_t)mlen); n += mlen;
                                                                r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                                                if (r != 1 || !result_ok()) {
                                                                    printf("AFP OpenMangled failed (%d)\n", r);
                                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                                             "FAIL: AFP OpenMangled failed (%d)\n", r);
                                                                    ok = false;
                                                                } else {
                                                                    fork_ref = (unsigned short)
                                                                        ((rx_buf[2] << 8) | rx_buf[3]);
                                                                    n = 2;
                                                                    cmd[0] = 27; cmd[1] = 0;
                                                                    cmd[n++] = (fork_ref >> 8) & 0xFF;
                                                                    cmd[n++] = fork_ref & 0xFF;
                                                                    cmd[n++] = 0; cmd[n++] = 0;
                                                                    cmd[n++] = 0; cmd[n++] = 0;
                                                                    cmd[n++] = 0; cmd[n++] = 0;
                                                                    cmd[n++] = 0; cmd[n++] = 64;
                                                                    cmd[n++] = 0; cmd[n++] = 0;
                                                                    r = afp_exchange(atp_ref, session_id, cmd, (short)n);
                                                                    if (r != 1 || !result_ok() || rx_len != 8
                                                                        || memcmp(rx_buf, "mangled!", 8) != 0) {
                                                                        printf("AFP ReadMangled failed (%d)\n", r);
                                                                        snprintf(fail_msg, sizeof(fail_msg),
                                                                                 "FAIL: AFP ReadMangled failed (%d)\n", r);
                                                                        ok = false;
                                                                    } else {
                                                                        printf("Mangled name round trip OK\n");
                                                                        cmd[0] = 4; cmd[1] = 0;
                                                                        cmd[2] = (fork_ref >> 8) & 0xFF;
                                                                        cmd[3] = fork_ref & 0xFF;
                                                                        r = afp_exchange(atp_ref, session_id, cmd, 4);
                                                                        if (r != 1 || !result_ok()) {
                                                                            printf("AFP CloseMangled failed (%d)\n", r);
                                                                            snprintf(fail_msg, sizeof(fail_msg),
                                                                                     "FAIL: AFP CloseMangled failed (%d)\n", r);
                                                                            ok = false;
                                                                        }
                                                                    }
                                                                }
                                                            }

                                                            /* FPCloseFork, FPCloseVol, FPLogout */
                                                            cmd[0] = 4; cmd[1] = 0;
                                                            cmd[2] = (fork_ref >> 8) & 0xFF;
                                                            cmd[3] = fork_ref & 0xFF;
                                                            r = afp_exchange(atp_ref, session_id, cmd, 4);
                                                            if (r != 1 || !result_ok()) {
                                                                printf("AFP CloseFork failed (%d)\n", r);
                                                                snprintf(fail_msg, sizeof(fail_msg),
                                                                         "FAIL: AFP CloseFork failed (%d)\n", r);
                                                                ok = false;
                                                            } else {
                                                                cmd[0] = 2; cmd[1] = 0;
                                                                cmd[2] = 0; cmd[3] = 1;
                                                                r = afp_exchange(atp_ref, session_id, cmd, 4);
                                                                if (r != 1 || !result_ok()) {
                                                                    printf("AFP CloseVol failed (%d)\n", r);
                                                                    snprintf(fail_msg, sizeof(fail_msg),
                                                                             "FAIL: AFP CloseVol failed (%d)\n", r);
                                                                    ok = false;
                                                                } else {
                                                                    cmd[0] = 20; cmd[1] = 0;
                                                                    r = afp_exchange(atp_ref, session_id, cmd, 2);
                                                                    if (r != 1 || !result_ok()) {
                                                                        printf("AFP Logout failed (%d)\n", r);
                                                                        snprintf(fail_msg, sizeof(fail_msg),
                                                                                 "FAIL: AFP Logout failed (%d)\n", r);
                                                                        ok = false;
                                                                    } else {
                                                                        printf("AFP session done\n");
                                                                        progress("stage: AFP session OK\n");
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            /* Tickle */
                            user[0] = 5;  /* Tickle */
                            user[1] = session_id;
                            user[2] = 0;
                            user[3] = 0;
                            r = asp_exchange(atp_ref, user, NULL, 0);
                            if (r != 1) {
                                printf("ASP tickle failed (%d)\n", r);
                                snprintf(fail_msg, sizeof(fail_msg),
                                         "FAIL: ASP tickle failed "
                                         "(%d)\n", r);
                                ok = false;
                            } else {
                                printf("ASP tickle OK\n");

                                /* CloseSess */
                                user[0] = 1;  /* CloseSess */
                                user[1] = session_id;
                                user[2] = 0;
                                user[3] = 0;
                                r = asp_exchange(atp_ref, user,
                                                 NULL, 0);
                                if (r != 1) {
                                    printf("ASP CloseSess failed "
                                           "(%d)\n", r);
                                    snprintf(fail_msg,
                                             sizeof(fail_msg),
                                             "FAIL: ASP CloseSess "
                                             "failed (%d)\n", r);
                                    ok = false;
                                } else {
                                    printf("ASP session closed\n");
                                    progress("stage: ASP session OK\n");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

done:
    if (ok) {
        printf("All %d packets echoed and NBP lookup succeeded\n", TEST_PACKETS);
        write_results("OK: all packets echoed\n");
    } else {
        write_results(fail_msg[0] ? fail_msg : "FAIL\n");
    }

    printf("Done. Press Return to quit.\n");
    getchar();
    return 0;
}
