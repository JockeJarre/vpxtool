This folder is for a small tool which will plant extra tags in the B2S Backglass XML files:
- .directb2s https://github.com/vpinball/b2s-backglass this is the original
- .b2spro https://github.com/KWildman69/B2S-Pro  this is the newer one with the same file type (xml) with some newer tags.

the tool should be able to go through a file folder and search for *.VPX recursivly.
If it finds a .directb2s file with a exact name like:

Blood Machine (VPW 2027).vpx
Blood Machine (VPW 2027).directb2s or .b2spro

Or with a similar name:
Blood Machine (VPW 2027) some version or author.vpx
Blood Machine (VPW 2027) different version or author.directb2s or .b2spro

It means exact or the same up to and including the (Company year) string is to count as exact.

now we need to investigate the directb2s file. If there is any filled tag called 
/DirectB2SData/GrillHeight@Value > 0 with a value > zero then the grill will be embedded in 
/DirectB2SData/Images/BackglassImage/@Value

so if we take the 

once these three prerequisits are met we can start investigate.
For the table file Blood Machine (VPW 2027).vpx
 - if there is a Blood Machine (VPW 2027).vbs file we skip to next section

 - else we need to use some library (check https://github.com/francisdb/vpxtool), to extract the vbs source code.
   into a temporary file

with the vbs file we need to find out which value the variable cGameName get from the source.
If none can be found it should be logged and go to next.

with the cGameName = VPinMame ROM name, we need to look up D:\vPinball\VPinMAME\DmdDevice.ini to see if there are any coordinates available. 
source code for VPinMame: D:\git\Flipper\pinmame

This is an example from Attack from Mars (Bally XXXX).vpx 

cGameName = afm_113b

[afm_113b]
virtualdmd left = 2972
virtualdmd top = 1526.66666666667
virtualdmd width = 886.666666666667
virtualdmd height = 265.333333333333

<DirectB2SData>
    ...
    </Images>
    <Displays>
        <DMD Target="Backglass" X="484" Y="1240" W="1034" H="218" GlassRoughness="0.1"/>
    </Displays>
</DirectB2SData>


AC-DC LUCI Premium VR (Stern 2013) v1.1.4:

cGameName = acd_170h

[acd_170h]
virtualdmd left = 4475
virtualdmd top = 2376
virtualdmd width = 1298
virtualdmd height = 428

embedded in the directb2s file:

<DirectB2SData>
    ...
    </Images>
    <Displays>
        <DMD Target="Backglass" X="616" Y="2060" W="1264" H="316" GlassRoughness="0.1"/>
    </Displays>
</DirectB2SData>

Both these examples can be found in D:\vPinball\VisualPinball\Tables\X9

The question is how to come
from left 4475 to 616
from top 2376 to 2060
from width 1298 to 1264
from height 428 to 316

this comes from the Pinmame values in DmdDevice.ini is when the backglass is resized for the installation.
The values in the directb2s is the coordinates within the backglass file.

One way of doing this 

One idea would be to fork https://github.com/francisdb/vpxtool into this folder, to get the vbs extract code and then continue from there.
