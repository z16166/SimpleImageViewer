#include "All.h"
#include "MACLib.h"
#include "CharacterHelper.h"
#include <stdlib.h>
#include <stddef.h>
#include <string.h>
#include <new>

using namespace APE;

extern "C" {

struct MonkeyDecoder {
    IAPEDecompress* pDecompress;
    MonkeyDecoder(IAPEDecompress* p) : pDecompress(p) {}
    ~MonkeyDecoder() { delete pDecompress; }
};

void* monkey_decoder_open(const str_utfn* filename) {
    int nErrorCode = 0;
    IAPEDecompress* pDecompress = CreateIAPEDecompress(filename, &nErrorCode, true, true, false);
    if (pDecompress == nullptr) {
        return nullptr;
    }
    
    MonkeyDecoder* decoder = new (std::nothrow) MonkeyDecoder(pDecompress);
    if (decoder == nullptr) {
        delete pDecompress;
        return nullptr;
    }
    return decoder;
}

void monkey_decoder_close(void* decoder) {
    if (decoder) {
        delete (MonkeyDecoder*)decoder;
    }
}

int monkey_decoder_get_info(void* decoder, int* sample_rate, int* bits_per_sample, int* channels, long long* total_blocks) {
    if (!decoder) return -1;
    MonkeyDecoder* d = (MonkeyDecoder*)decoder;
    
    if (sample_rate) *sample_rate = (int)d->pDecompress->GetInfo(APE::IAPEDecompress::APE_INFO_SAMPLE_RATE);
    if (bits_per_sample) *bits_per_sample = (int)d->pDecompress->GetInfo(APE::IAPEDecompress::APE_INFO_BITS_PER_SAMPLE);
    if (channels) *channels = (int)d->pDecompress->GetInfo(APE::IAPEDecompress::APE_INFO_CHANNELS);
    if (total_blocks) *total_blocks = (long long)d->pDecompress->GetInfo(APE::IAPEDecompress::APE_DECOMPRESS_TOTAL_BLOCKS);
    
    return 0;
}

int monkey_decoder_decode_blocks(void* decoder, unsigned char* buffer, int blocks_to_decode, int* blocks_decoded) {
    if (!decoder) return -1;
    MonkeyDecoder* d = (MonkeyDecoder*)decoder;
    
    int64 nBlocksDecoded = 0;
    int nRet = d->pDecompress->GetData(buffer, blocks_to_decode, &nBlocksDecoded);
    
    if (blocks_decoded) *blocks_decoded = (int)nBlocksDecoded;
    return nRet;
}

int monkey_decoder_seek(void* decoder, long long block_offset) {
    if (!decoder) return -1;
    MonkeyDecoder* d = (MonkeyDecoder*)decoder;
    return d->pDecompress->Seek(block_offset);
}

}
